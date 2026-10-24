//! The ignore-blind parallel walker.
//!
//! Traversal is the hot path — classification, policy and reporting are noise beside the cost
//! of enumerating a few million directory entries — so this module is where the wall-clock
//! goes. See `adrs/0005-traversal-and-parallelism.md`.
//!
//! Two things carry the performance, and both are about *not walking* rather than walking
//! faster: every ignore mechanism is off (so nothing is missed and no ignore file is parsed),
//! and whole subtrees are pruned at the directory level (so the largest, deepest parts of a
//! disk are never entered).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ignore::WalkBuilder;

use crate::caches::CacheRoots;
use crate::classify::{Classifier, Finding, SkipReason};

/// Directories that are never descended into and never classified, whatever else is true of
/// them.
///
/// A VCS directory is not an artifact and its contents are irreplaceable, so it is out of reach
/// of every flag — `--clean-dependencies` included. Dependency caches are pruned just as hard
/// but are *not* in this list: they are classified at the directory itself before the prune, so
/// that the opt-in artifacts declared for them (ADR 0001's amendment) can fire.
pub const NEVER_DESCEND: &[&str] = &[".git", ".hg", ".svn"];

mod guided;
mod patterns;
mod visitor;

pub use patterns::PatternSet;
use visitor::Visitor;

/// How to walk.
#[expect(
    clippy::struct_excessive_bools,
    reason = "these mirror command-line flags, which are bools by nature"
)]
#[derive(Debug)]
pub struct ScanOptions {
    /// Worker threads. `None` means available parallelism.
    ///
    /// Saturating cores is the right default on an SSD and the wrong one on a spinning disk or
    /// a network filesystem, which is why `-j` exists.
    pub jobs: Option<usize>,
    /// Whether to stay on the scan root's filesystem. Mounted volumes, network shares and
    /// external disks are outside the intent of "clean this tree" (ADR 0006).
    pub one_file_system: bool,
    /// Whether to record why candidates were passed over. Costs memory proportional to the
    /// number of near-misses, so the default view does not ask for it.
    pub collect_skips: bool,
    /// Paths never scanned.
    pub exclude: PatternSet,
    /// Paths swept without a marker proving them — the user said so outright (ADR 0004).
    pub include: PatternSet,
    /// Tool caches and installed toolchains never descended into (ADR 0001).
    pub caches: CacheRoots,
    /// Whether to collect the repositories the walk passes, for git housekeeping (ADR 0011).
    pub collect_repositories: bool,
    /// Whether a directory carrying a valid `CACHEDIR.TAG` is an artifact (ADR 0013).
    ///
    /// Off by default and reached only by asking. When on, such a directory is taken whole and
    /// pruned — which is *less* work than the alternative, because a relocated build directory is
    /// otherwise descended in full and yields nothing but skips.
    pub clean_tagged: bool,
    /// Whether to walk a git work tree from its index where the filesystem allows (`guided`).
    pub guide_repositories: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            jobs: None,
            one_file_system: true,
            collect_skips: false,
            exclude: PatternSet::default(),
            include: PatternSet::default(),
            caches: CacheRoots::default(),
            collect_repositories: false,
            clean_tagged: false,
            guide_repositories: true,
        }
    }
}

/// A candidate that was passed over, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// The path not taken.
    pub path: PathBuf,
    /// Why not.
    pub reason: SkipReason,
}

/// Something went wrong walking, on one path, without stopping the run.
#[derive(Debug, Clone)]
pub struct WalkFailure {
    /// The path involved, when the walker knew one.
    pub path: Option<PathBuf>,
    /// What happened.
    pub message: String,
    /// Whether this was a transient interruption rather than a real inability to read.
    ///
    /// A sweep of `$HOME` on macOS produces a handful of `EINTR`s every single time, under
    /// `~/Library/Containers`. Listing them beside genuine permission and IO failures teaches
    /// the reader to skim past the section where those genuine failures appear, so they are
    /// separated. See [`WalkFailure::transient`].
    pub transient: bool,
    /// Whether the user simply may not read this directory: `EPERM` or `EACCES`.
    ///
    /// On macOS a `$HOME` sweep meets a hundred-odd of these under `~/Library`, protected by the
    /// system's privacy controls and not by anything the user did wrong. They are expected, not
    /// actionable, and the report counts them in one line instead of listing each (`-v` does).
    pub inaccessible: bool,
}

/// How many times an interrupted directory read is attempted again before it is reported.
const INTERRUPTED_RETRIES: usize = 3;

/// What one walk found.
#[derive(Debug, Default)]
pub struct Scan {
    /// Proven artifacts, in walk order — the reporter sorts (ADR 0007).
    pub findings: Vec<Finding>,
    /// Candidates passed over, when `collect_skips` was set.
    pub skips: Vec<Skipped>,
    /// How many candidates were passed over, always counted even when not collected.
    pub skipped_count: usize,
    /// Per-path walk failures. Reported, never fatal.
    pub failures: Vec<WalkFailure>,
    /// Working trees of the repositories the walk passed, when `collect_repositories` was set
    /// (ADR 0011). Not deduplicated here — resolving a linked worktree to its object store is
    /// the git module's job, and it needs the paths as walked.
    pub repositories: Vec<PathBuf>,
}

enum Message {
    Found(Box<Finding>),
    Skipped(Box<Skipped>),
    SkippedCount,
    Failure(Box<WalkFailure>),
    Repository(PathBuf),
    /// A work tree to walk from its index rather than its directories.
    Guide(PathBuf),
}

/// Walks every root and classifies what it finds.
///
/// Each root is walked separately so that containment and the marker-climb bound are evaluated
/// against the root the entry actually came from; within a root the walk is fully parallel.
#[must_use]
pub fn scan(roots: &[PathBuf], classifier: &Classifier, options: &ScanOptions) -> Scan {
    let mut scan = Scan::default();
    for root in roots {
        scan_root(root, classifier, options, &mut scan);
    }
    scan
}

fn scan_root(root: &Path, classifier: &Classifier, options: &ScanOptions, scan: &mut Scan) {
    let first_failure = scan.failures.len();
    walk_into(root, root, classifier, options, scan);
    let found = scan.findings.len();
    retry_interrupted(root, classifier, options, scan, first_failure);
    // A re-walk of an interrupted directory can re-emit an artifact the first walk had already
    // reported before the read error reached the collector. The same path would then be taken
    // twice, and the second removal refused as vanished and counted as a safety refusal it never
    // was — a wrong statement in a destructive tool's only record.
    dedupe_findings(scan, found);
}

/// Drops findings a retry emitted for a path that was already reported, keeping the first.
fn dedupe_findings(scan: &mut Scan, from: usize) {
    if scan.findings.len() <= from {
        return;
    }
    let mut seen: HashSet<PathBuf> = scan.findings[..from]
        .iter()
        .map(|finding| finding.path.clone())
        .collect();
    let retried = scan.findings.split_off(from);
    scan.findings
        .extend(retried.into_iter().filter(|finding| seen.insert(finding.path.clone())));
}

/// Walks `start`, classifying against `root`, and appends what it finds to `scan`.
///
/// Work trees are handed to `guided`, which walks them from their index and returns the
/// untracked directories still to walk; those are walked here in turn, until nothing is left.
fn walk_into(start: &Path, root: &Path, classifier: &Classifier, options: &ScanOptions, scan: &mut Scan) {
    let visitor = Visitor {
        root,
        classifier,
        options,
        guide: options.guide_repositories && guided::supported(root),
    };
    // A root that is itself a work tree is never offered to the visitor as an entry.
    let mut work_trees = Vec::new();
    let mut starts = vec![start.to_path_buf()];
    if visitor.guide && start == root && guided::is_work_tree(start) {
        work_trees = std::mem::take(&mut starts);
    }
    loop {
        if !starts.is_empty() {
            let (sender, receiver) = mpsc::channel();
            let mut builder = WalkBuilder::new(&starts[0]);
            for more in &starts[1..] {
                builder.add(more);
            }
            configure(&mut builder, options).build_parallel().run(|| {
                let sender = sender.clone();
                Box::new(move |result| visitor.visit(result, &sender))
            });
            drop(sender);
            collect(receiver, scan, &mut work_trees);
        }
        if work_trees.is_empty() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        starts = guided::walk(visitor, &std::mem::take(&mut work_trees), &sender);
        drop(sender);
        collect(receiver, scan, &mut work_trees);
    }
}

/// Folds every message a walk sent into `scan`, setting work trees aside for the guided walk.
fn collect(receiver: mpsc::Receiver<Message>, scan: &mut Scan, work_trees: &mut Vec<PathBuf>) {
    for message in receiver {
        match message {
            Message::Found(finding) => scan.findings.push(*finding),
            Message::Skipped(skipped) => {
                scan.skipped_count += 1;
                scan.skips.push(*skipped);
            }
            Message::SkippedCount => scan.skipped_count += 1,
            Message::Failure(failure) => scan.failures.push(*failure),
            Message::Repository(path) => scan.repositories.push(path),
            Message::Guide(path) => work_trees.push(path),
        }
    }
}

/// Walks again every directory whose read was interrupted by a signal, from `scan.failures[from..]`.
///
/// `EINTR` says a signal landed mid-read, not that the directory cannot be read, and the walker
/// offers no way to resume the entry it gave up on — so the directory is walked afresh as its own
/// subtree, classified against the same root. One that is interrupted again is retried, a few
/// times, and only then left in the report as the interruption it is.
fn retry_interrupted(root: &Path, classifier: &Classifier, options: &ScanOptions, scan: &mut Scan, from: usize) {
    for _ in 0..INTERRUPTED_RETRIES {
        let mut pending = Vec::new();
        let mut kept = Vec::new();
        for failure in scan.failures.split_off(from) {
            match failure.path.clone().filter(|_| failure.transient) {
                Some(path) => pending.push(path),
                None => kept.push(failure),
            }
        }
        scan.failures.extend(kept);
        if pending.is_empty() {
            return;
        }
        for path in pending {
            walk_into(&path, root, classifier, options, scan);
        }
    }
}

/// Configures the walker.
///
/// ~keep Every one of these settings is load-bearing and none of them may be re-enabled.
///
/// Build artifacts are precisely what `.gitignore` lists. A walker that respects ignore files
/// skips `target/`, `dist/`, `__pycache__/` and `.venv/` — which is to say it skips everything
/// voom exists to find, reports a clean machine, and reclaims nothing. That failure mode is
/// silent: no user would report it as a bug, they would conclude their disk was already clean.
/// `hidden(false)` matters for the same reason — `.venv`, `.gradle`, `.zig-cache` and
/// `.terraform` are all hidden.
///
/// See `adrs/0005-traversal-and-parallelism.md`, and `should_find_a_gitignored_target` below,
/// which exists to catch exactly this and has been verified to fail when it is broken.
fn configure<'b>(builder: &'b mut WalkBuilder, options: &ScanOptions) -> &'b mut WalkBuilder {
    builder
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .hidden(false)
        .follow_links(false)
        .same_file_system(options.one_file_system);
    builder.threads(walker_threads(options.jobs));
    builder
}

/// How many walker threads to run.
///
/// An explicit `-j` is taken as given. Otherwise the walk oversubscribes the cores: it is a
/// `getdirentries` and an `lstat` per entry, which is time spent waiting on the filesystem and
/// not computing, so a thread per core leaves the disk idle between requests. The `ignore`
/// crate's own default stops at 12 whatever the machine has. Measured on one 18-core machine under
/// load, a lone thread took 8.5 minutes over a tree that the default took 35 seconds on.
fn walker_threads(jobs: Option<usize>) -> usize {
    jobs.map_or_else(crate::limits::default_width, |jobs| jobs.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{ArtifactId, Provenance, Selection};
    use crate::testing::tree;

    fn classifier() -> Classifier {
        Classifier::new(Selection::all()).expect("the catalog compiles")
    }

    #[test]
    fn should_drop_a_finding_a_retry_re_emitted() {
        let finding = |path: &str| Finding {
            path: PathBuf::from(path),
            provenance: Provenance::Included {
                pattern: "x".to_owned(),
            },
        };
        let mut scan = Scan::default();
        scan.findings.push(finding("/root/target"));
        let from = scan.findings.len();
        scan.findings.push(finding("/root/target"));
        scan.findings.push(finding("/root/target-twin"));

        dedupe_findings(&mut scan, from);

        let paths: Vec<_> = scan
            .findings
            .iter()
            .map(|finding| finding.path.display().to_string())
            .collect();
        assert_eq!(paths, ["/root/target", "/root/target-twin"]);
    }

    fn found(scan: &Scan, root: &Path) -> Vec<String> {
        let mut paths: Vec<String> = scan
            .findings
            .iter()
            .map(|finding| {
                // `/` on every platform: these are compared against `/`-separated literals,
                // and `display()` renders the platform separator.
                finding
                    .path
                    .strip_prefix(root)
                    .unwrap_or(&finding.path)
                    .display()
                    .to_string()
                    .replace(std::path::MAIN_SEPARATOR, "/")
            })
            .collect();
        paths.sort();
        paths
    }

    fn run(root: &Path, options: &ScanOptions) -> Scan {
        scan(&[root.to_path_buf()], &classifier(), options)
    }

    fn verbose() -> ScanOptions {
        ScanOptions {
            collect_skips: true,
            ..ScanOptions::default()
        }
    }

    /// ADR 0004 makes `include` the only route to unanchored deletion: no marker proves the
    /// path, the user named it.
    #[test]
    fn should_take_an_included_path_that_no_marker_proves() {
        let fixture = tree(&["junk/leftovers.o"]);
        let options = PatternSet::new([format!("{}/junk", fixture.path().display())])
            .map(|include| ScanOptions { include, ..verbose() })
            .expect("the include compiles");

        let scan = run(fixture.path(), &options);

        assert_eq!(found(&scan, fixture.path()), vec!["junk"]);
        assert!(
            matches!(&scan.findings[0].provenance, Provenance::Included { pattern } if pattern.ends_with("junk")),
            "the pattern is carried so the report can say what asked for this"
        );
    }

    /// ADR 0004: `exclude` is absolute and cannot be overridden by a more specific `include`.
    #[test]
    fn should_let_exclude_beat_include_on_the_same_path() {
        let fixture = tree(&["junk/leftovers.o"]);
        let pattern = format!("{}/junk", fixture.path().display());
        let options = ScanOptions {
            exclude: PatternSet::new([pattern.clone()]).expect("it compiles"),
            include: PatternSet::new([pattern]).expect("it compiles"),
            ..verbose()
        };

        let scan = run(fixture.path(), &options);

        assert!(scan.findings.is_empty(), "exclude wins, whatever else names the path");
    }

    /// Dependency directories are kept out of the catalog on purpose (ADR 0001), so naming one
    /// explicitly is the only way to reach it — which means `include` has to outrank the prune.
    #[test]
    fn should_let_an_include_reach_into_a_dependency_directory() {
        let fixture = tree(&["node_modules/left-pad/index.js"]);
        let options = PatternSet::new([format!("{}/node_modules", fixture.path().display())])
            .map(|include| ScanOptions { include, ..verbose() })
            .expect("the include compiles");

        let scan = run(fixture.path(), &options);

        assert_eq!(found(&scan, fixture.path()), vec!["node_modules"]);
    }

    /// `include` outranks the cache skip the same way it outranks the dependency prune: it can
    /// name the pruned directory itself.
    #[test]
    fn should_let_an_include_name_a_tool_cache_outright() {
        let fixture = tree(&[".npm/_cacache/pkg/leftovers.o"]);
        let options = ScanOptions {
            caches: CacheRoots::under(fixture.path(), fixture.path()),
            include: PatternSet::new([format!("{}/.npm", fixture.path().display())]).expect("the include compiles"),
            ..verbose()
        };

        let scan = run(fixture.path(), &options);

        assert_eq!(found(&scan, fixture.path()), vec![".npm"]);
    }

    /// What it cannot do is reach *inside* one, and that is worth pinning rather than leaving
    /// to be discovered. Pruning happens at the directory level — that is where all the
    /// wall-clock saving comes from — so once the walk declines to descend into `.npm`, nothing
    /// below it is ever offered to any matcher. Deciding otherwise would mean asking, at every
    /// prune, whether some glob might match an unknown descendant, which globs cannot answer.
    ///
    /// It fails toward not deleting, which is the direction `safety-first` asks for. A user who
    /// wants a path inside a cache swept names the cache as a scan root.
    #[test]
    fn should_not_let_an_include_reach_inside_a_pruned_directory() {
        let fixture = tree(&[".npm/_cacache/pkg/leftovers.o"]);
        let options = ScanOptions {
            caches: CacheRoots::under(fixture.path(), fixture.path()),
            include: PatternSet::new([format!("{}/.npm/_cacache/pkg", fixture.path().display())])
                .expect("the include compiles"),
            ..verbose()
        };

        let scan = run(fixture.path(), &options);

        assert!(
            scan.findings.is_empty(),
            "an include names a path, not an artifact to sweep"
        );
        assert!(fixture.path().join(".npm/_cacache/pkg/leftovers.o").exists());
    }

    /// An installed toolchain is shaped exactly like a project — `~/.cache/node/corepack` really
    /// does hold a `package.json` beside a `dist/` — so the classifier proves it and would take
    /// it. Only its location distinguishes an installed program from a built one.
    #[test]
    fn should_never_descend_into_a_tool_cache() {
        let fixture = tree(&[
            ".npm/_cacache/pkg/package.json",
            ".npm/_cacache/pkg/dist/bundle.js",
            "project/package.json",
            "project/dist/bundle.js",
        ]);
        let options = ScanOptions {
            caches: CacheRoots::under(fixture.path(), fixture.path()),
            ..verbose()
        };

        let scan = run(fixture.path(), &options);

        assert_eq!(found(&scan, fixture.path()), vec!["project/dist"]);
        assert!(
            scan.skips.iter().any(|skip| skip.reason == SkipReason::ToolCache),
            "the skip is reported, not silent"
        );
    }

    /// A named cache is proven the way everything else is. The location says which directory
    /// to look at; the marker inside says whether it may go. Without the marker it stays, and
    /// this is the data-loss regression test for the whole cache table.
    #[test]
    fn should_take_a_named_cache_only_when_its_marker_is_inside_it() {
        let fixture = tree(&[
            ".npm/_cacache/content-v2/blob",
            ".npm/_cacache/index-v5/entry",
            ".cargo/registry/src/crate.rs",
        ]);

        let scan = scan(
            &[fixture.path().to_path_buf()],
            &classifier(),
            &ScanOptions {
                // `.npm/_cacache` has both its markers; `.cargo/registry` has no CACHEDIR.TAG.
                caches: CacheRoots::under_with(
                    fixture.path(),
                    fixture.path(),
                    &["npm".to_owned(), "cargo-registry".to_owned()],
                ),
                ..verbose()
            },
        );

        assert_eq!(
            found(&scan, fixture.path()),
            vec![".npm/_cacache".to_owned()],
            "the proven cache is taken and the unproven one is not"
        );
        assert!(
            scan.skips.iter().any(|skipped| skipped.path.ends_with("registry")
                && matches!(skipped.reason, SkipReason::NoInnerMarker { .. })),
            "and the unproven one says why: {:?}",
            scan.skips
        );
    }

    /// A directory carrying an `Inside`-anchored artifact's name but not its marker is pruned
    /// rather than descended into.
    ///
    /// A deliberate recall trade, and the only skip reason that prunes. The directory is either
    /// an older cache of that tool or somebody else's directory of the same name; either way
    /// voom has already declined to remove it, and walking it can only surface *other* projects
    /// nested inside a cache. Measured over one home directory: zero artifacts found that way,
    /// 181 of 2305 skips generated, and the scan five times slower.
    ///
    /// Only reaches artifacts that are on by default. An off-by-default one reports
    /// `NotEnabled` before the marker is consulted, and `NotEnabled` must never prune — it
    /// fires for `build/` and `bin/` too, where descending is the whole point.
    #[test]
    fn should_not_walk_inside_an_unproven_inside_anchored_candidate() {
        let fixture = tree(&[
            // No `CACHEDIR.TAG`, so nothing proves it is alef's.
            ".alef/blob",
            ".alef/nested/Cargo.toml",
            ".alef/nested/target/build.o",
        ]);

        let scan = run(fixture.path(), &verbose());

        assert!(
            scan.findings.is_empty(),
            "the interior of an unproven cache is not walked: {:?}",
            found(&scan, fixture.path())
        );
        assert!(
            scan.skips
                .iter()
                .any(|skipped| matches!(skipped.reason, SkipReason::NoInnerMarker { .. })),
            "and the directory itself is reported, so the reader knows why: {:?}",
            scan.skips
        );
    }

    /// The other half: a *proven* one is not walked either, because a matched artifact is an
    /// indivisible unit. Same outcome, different reason, and both must hold.
    #[test]
    fn should_not_walk_inside_a_proven_inside_anchored_artifact() {
        let fixture = tree(&[
            ".alef/CACHEDIR.TAG",
            ".alef/nested/Cargo.toml",
            ".alef/nested/target/build.o",
        ]);

        let scan = run(fixture.path(), &verbose());

        assert_eq!(
            found(&scan, fixture.path()),
            vec![".alef".to_owned()],
            "the artifact itself, and nothing from inside it"
        );
    }

    /// Naming a cache reaches it through the ancestor that would otherwise prune it — without
    /// opening that ancestor up, which is what `CACHE_DIRS` exists to prevent.
    #[test]
    fn should_reach_a_named_cache_inside_a_pruned_ancestor() {
        let fixture = tree(&[
            ".npm/_cacache/content-v2/blob",
            // A project shape elsewhere under the same pruned ancestor. Reaching the cache must
            // not also let the classifier loose on this.
            ".npm/_npx/installed/package.json",
            ".npm/_npx/installed/dist/bundle.js",
        ]);

        let scan = scan(
            &[fixture.path().to_path_buf()],
            &classifier(),
            &ScanOptions {
                caches: CacheRoots::under_with(fixture.path(), fixture.path(), &["npm".to_owned()]),
                ..verbose()
            },
        );

        assert_eq!(
            found(&scan, fixture.path()),
            vec![".npm/_cacache".to_owned()],
            "the named cache, and nothing else inside the pruned ancestor"
        );
    }

    /// The negative: the same tree with nothing named leaves every cache alone.
    #[test]
    fn should_leave_every_cache_alone_when_none_is_named() {
        let fixture = tree(&[".npm/_cacache/content-v2/blob", ".npm/_cacache/index-v5/entry"]);

        let scan = scan(
            &[fixture.path().to_path_buf()],
            &classifier(),
            &ScanOptions {
                caches: CacheRoots::under_with(fixture.path(), fixture.path(), &[]),
                ..verbose()
            },
        );

        assert!(scan.findings.is_empty(), "{:?}", found(&scan, fixture.path()));
    }

    /// The regression test the walker configuration exists for.
    ///
    /// Build artifacts are precisely what `.gitignore` lists. If this ever fails, one of the
    /// ignore mechanisms in `scan_root` was re-enabled and voom now reports a clean machine and
    /// reclaims nothing. Fix the walker, never this test — see CONTRIBUTING.
    #[test]
    fn should_find_a_gitignored_target() {
        // The `.git/HEAD` is load-bearing in the *fixture*: the `ignore` crate only honours a
        // `.gitignore` inside something it recognises as a repository, so without it this test
        // passes even with `git_ignore(true)` and proves nothing. Verified by flipping the
        // setting and watching this fail.
        let fixture = tree(&[".git/HEAD", ".gitignore", "Cargo.toml", "target/", "target/debug/"]);
        std::fs::write(fixture.path().join(".gitignore"), b"/target\n").expect("a .gitignore");
        let scan = run(fixture.path(), &ScanOptions::default());
        assert_eq!(found(&scan, fixture.path()), vec!["target".to_owned()]);
    }

    /// `.venv`, `.gradle`, `.zig-cache` and `.terraform` are all hidden. A walker that skips
    /// hidden entries misses most of the catalog.
    #[test]
    fn should_find_a_hidden_artifact() {
        let fixture = tree(&["build.zig", ".zig-cache/"]);
        let scan = run(fixture.path(), &ScanOptions::default());
        assert_eq!(found(&scan, fixture.path()), vec![".zig-cache".to_owned()]);
    }

    /// A matched artifact is an indivisible unit. Descending into it would waste the walk and
    /// produce nested findings that make the report unreadable.
    #[test]
    fn should_not_descend_into_a_matched_artifact() {
        let fixture = tree(&[
            "Cargo.toml",
            "target/",
            "target/nested/Cargo.toml",
            "target/nested/target/",
        ]);
        let scan = run(fixture.path(), &ScanOptions::default());
        assert_eq!(found(&scan, fixture.path()), vec!["target".to_owned()]);
    }

    #[test]
    fn should_not_descend_into_a_vcs_directory() {
        let fixture = tree(&[".git/Cargo.toml", ".git/target/", "README.md"]);
        let scan = run(fixture.path(), &ScanOptions::default());
        assert!(
            found(&scan, fixture.path()).is_empty(),
            "nothing inside .git is voom's business"
        );
    }

    /// Dependency caches are out of scope (ADR 0001) and are also the deepest subtrees on a
    /// typical disk, so not descending into them is most of the performance story.
    #[test]
    fn should_not_descend_into_a_dependency_directory() {
        let fixture = tree(&[
            "package.json",
            "node_modules/pkg/Cargo.toml",
            "node_modules/pkg/target/",
        ]);
        let scan = run(fixture.path(), &ScanOptions::default());
        assert!(
            found(&scan, fixture.path()).is_empty(),
            "a dependency directory is never a finding by default"
        );
    }

    #[test]
    fn should_prune_an_excluded_subtree() {
        let fixture = tree(&["keep/Cargo.toml", "keep/target/", "drop/Cargo.toml", "drop/target/"]);
        let exclude = PatternSet::new([format!("{}/drop/**", fixture.path().display())]).expect("a glob");
        let options = ScanOptions { exclude, ..verbose() };
        let scan = run(fixture.path(), &options);
        assert_eq!(found(&scan, fixture.path()), vec!["keep/target".to_owned()]);
        assert!(
            scan.skips
                .iter()
                .any(|skip| matches!(skip.reason, SkipReason::Excluded { .. })),
            "an exclusion must be reported, not silently applied"
        );
    }

    /// A symlinked `target/` pointing at a source tree must never become a deletion of that
    /// source tree — and the user has to be *told* it is there, because a link standing where an
    /// artifact belongs is the shape of a mistake worth looking at.
    ///
    /// It used to be turned into a skip here, which meant it appeared as part of an anonymous
    /// count unless `-v` was passed. It is a finding now, and the deletion guard refuses it by
    /// name in the artifacts block. The refusal, not this module, is what keeps the target safe;
    /// [`should_never_delete_through_a_symlink`](../../tests/safety.rs) is the test for that.
    #[test]
    #[cfg(unix)]
    fn should_report_a_symlinked_artifact_as_a_finding() {
        let fixture = tree(&["Cargo.toml", "elsewhere/precious.txt"]);
        std::os::unix::fs::symlink(fixture.path().join("elsewhere"), fixture.path().join("target")).expect("a symlink");

        let scan = run(fixture.path(), &verbose());

        assert_eq!(
            found(&scan, fixture.path()),
            vec!["target".to_owned()],
            "the link is named rather than counted"
        );
        assert!(
            !scan.skips.iter().any(|skip| skip.reason == SkipReason::Symlink),
            "and is not also folded into the skips"
        );
        assert!(fixture.path().join("elsewhere/precious.txt").exists());
    }

    /// The walk stops at a symlinked artifact rather than enumerating what it points at, so a
    /// link to a large tree costs one entry and the report gains no nested findings.
    #[test]
    #[cfg(unix)]
    fn should_not_walk_inside_a_symlinked_artifact() {
        let fixture = tree(&[
            "Cargo.toml",
            "elsewhere/nested/Cargo.toml",
            "elsewhere/nested/target/app",
        ]);
        std::os::unix::fs::symlink(fixture.path().join("elsewhere"), fixture.path().join("target")).expect("a symlink");

        let scan = run(fixture.path(), &verbose());

        assert_eq!(
            found(&scan, fixture.path()),
            vec!["elsewhere/nested/target".to_owned(), "target".to_owned()],
            "the real nested artifact is found by its own path, and nothing through the link"
        );
    }

    /// Ruby's `vendor/bundle/` and Julia's `deps/build/` sit inside directories the walker
    /// prunes. Without the targeted check they would be permanently unreachable — a catalog
    /// entry that can never fire.
    #[test]
    fn should_reach_an_artifact_declared_inside_a_pruned_dependency_directory() {
        let fixture = tree(&["Gemfile", "vendor/bundle/gems/rake.rb", "vendor/other/keep.rb"]);
        // Opt-in since it is an installed-gem directory, so the run has to name it.
        let mut selection = Selection::all();
        assert!(selection.set("ruby.vendor-bundle", true), "the spec exists");

        let scan = scan(
            &[fixture.path().to_path_buf()],
            &Classifier::new(selection).expect("the catalog compiles"),
            &ScanOptions::default(),
        );

        assert_eq!(found(&scan, fixture.path()), vec!["vendor/bundle".to_owned()]);
    }

    #[test]
    fn should_still_not_walk_the_rest_of_a_dependency_directory() {
        let fixture = tree(&[
            "Gemfile",
            "package.json",
            "vendor/nested/Cargo.toml",
            "vendor/nested/target/app",
        ]);
        let scan = run(fixture.path(), &ScanOptions::default());
        assert!(
            found(&scan, fixture.path()).is_empty(),
            "nothing else inside vendor/ is walked"
        );
    }

    /// The performance property `--clean-dependencies` had to be built around: a dependency
    /// directory that is *going to be deleted* is still never enumerated.
    ///
    /// Two independent assertions, because either alone can pass while the walk is wrong. The
    /// nested `target/` proves no entry below `node_modules/` reached the classifier at all.
    /// The probe count proves it more precisely: marker resolution is the one place
    /// classification touches the filesystem, and one probe is the project directory itself —
    /// the anchor `node_modules/` resolves against. A walk that descended would probe
    /// `node_modules/pkg` for the nested `target/` too.
    #[test]
    fn should_not_enumerate_inside_an_enabled_dependency_directory() {
        let fixture = tree(&[
            "package.json",
            "node_modules/pkg/Cargo.toml",
            "node_modules/pkg/target/app",
        ]);
        let mut selection = Selection::all();
        assert!(selection.set("node.node_modules", true));
        let classifier = Classifier::new(selection).expect("the catalog compiles");

        let scan = scan(&[fixture.path().to_path_buf()], &classifier, &verbose());

        assert_eq!(
            found(&scan, fixture.path()),
            vec!["node_modules".to_owned()],
            "the directory itself is taken, and nothing inside it is even seen"
        );
        assert_eq!(
            classifier.probe_count(),
            1,
            "one anchor listing — the project directory — and none below the prune"
        );
    }

    /// With the opt-in off, the same tree yields nothing at all, and the skip says which spec
    /// would change that.
    #[test]
    fn should_explain_a_dependency_directory_it_did_not_take() {
        let fixture = tree(&["package.json", "node_modules/left-pad/index.js"]);
        let scan = run(fixture.path(), &verbose());

        assert!(
            found(&scan, fixture.path()).is_empty(),
            "a dependency directory is never a finding by default"
        );
        assert!(
            scan.skips.iter().any(|skip| matches!(
                &skip.reason,
                SkipReason::NotEnabled { spec, note } if spec == "node.node_modules" && note.is_some()
            )),
            "a pruned dependency directory now explains its opt-in, got {:?}",
            scan.skips
        );
    }

    #[test]
    fn should_not_report_a_nested_dependency_artifact_without_its_marker() {
        let fixture = tree(&["vendor/bundle/gems/rake.rb"]);
        let scan = run(fixture.path(), &verbose());
        assert!(found(&scan, fixture.path()).is_empty(), "no Gemfile proves it");
        assert!(fixture.path().join("vendor/bundle/gems/rake.rb").exists());
    }

    #[test]
    fn should_scan_several_roots() {
        let first = tree(&["Cargo.toml", "target/"]);
        let second = tree(&["package.json", "dist/"]);
        let roots = vec![first.path().to_path_buf(), second.path().to_path_buf()];
        let scan = scan(&roots, &classifier(), &ScanOptions::default());
        assert_eq!(scan.findings.len(), 2);
    }

    #[test]
    fn should_count_skips_without_collecting_them_by_default() {
        let fixture = tree(&["target/", "dist/"]);
        let scan = run(fixture.path(), &ScanOptions::default());
        assert!(scan.findings.is_empty(), "neither artifact is on by default");
        assert_eq!(scan.skipped_count, 2);
        assert!(scan.skips.is_empty(), "the default view keeps the report short");
    }

    #[test]
    fn should_collect_skip_reasons_when_asked() {
        let fixture = tree(&["target/"]);
        let scan = run(fixture.path(), &verbose());
        assert_eq!(scan.skips.len(), 1);
        assert!(matches!(scan.skips[0].reason, SkipReason::NoMarker { .. }));
    }

    #[test]
    fn should_find_the_root_artifact_but_never_the_root_itself() {
        let fixture = tree(&["Cargo.toml", "target/"]);
        let scan = run(fixture.path(), &ScanOptions::default());
        assert!(scan.findings.iter().all(|finding| finding.path != fixture.path()));
    }

    #[test]
    fn should_record_the_declaration_that_proved_each_finding() {
        let fixture = tree(&["Cargo.toml", "target/"]);
        let scan = run(fixture.path(), &ScanOptions::default());
        let finding = &scan.findings[0];
        let id: ArtifactId = finding.artifact().expect("a marker proved it");
        assert_eq!(id.ecosystem().id, "rust");
        assert!(matches!(
            &finding.provenance,
            Provenance::Anchored { marker_dir, .. } if marker_dir == fixture.path()
        ));
    }

    #[test]
    fn an_empty_exclude_set_matches_nothing() {
        let excludes = PatternSet::default();
        assert!(excludes.is_empty());
        assert_eq!(excludes.matched(Path::new("/anything")), None);
    }

    /// The walker wraps a read failure in a depth marker, and the path it names was being
    /// dropped on the way out, so the JSON carried `null` and the path only in the message.
    #[test]
    #[cfg(unix)]
    fn should_name_the_directory_it_cannot_read_and_call_it_inaccessible() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tree(&["locked/inside.txt", "open/inside.txt"]);
        let locked = fixture.path().join("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("a mode");
        let readable = std::fs::read_dir(&locked).is_ok();

        let scan = run(fixture.path(), &verbose());

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("a mode");
        if readable {
            return; // running as root: nothing to refuse
        }
        assert_eq!(scan.failures.len(), 1, "{:?}", scan.failures);
        assert_eq!(scan.failures[0].path.as_deref(), Some(locked.as_path()));
        assert!(scan.failures[0].inaccessible);
        assert!(!scan.failures[0].transient);
    }

    /// `EINTR` is a signal landing mid-read, not a verdict on the directory. The walker cannot
    /// resume the entry, so the scan walks the directory again itself rather than reporting it.
    #[test]
    fn should_walk_a_directory_again_when_its_read_was_interrupted() {
        let fixture = tree(&["app/Cargo.toml", "app/target/debug/out"]);
        let root = fixture.path();
        let mut scan = Scan {
            failures: vec![WalkFailure {
                path: Some(root.join("app")),
                message: "Interrupted system call (os error 4)".to_owned(),
                transient: true,
                inaccessible: false,
            }],
            ..Scan::default()
        };
        let visitor_root = root.to_path_buf();

        retry_interrupted(&visitor_root, &classifier(), &verbose(), &mut scan, 0);

        assert!(scan.failures.is_empty(), "{:?}", scan.failures);
        assert_eq!(found(&scan, root), vec!["app/target"]);
    }

    /// A directory that keeps failing is still reported, as the interruption it is, rather than
    /// retried forever.
    #[test]
    fn should_give_up_on_a_directory_that_stays_interrupted() {
        let fixture = tree(&["app/Cargo.toml"]);
        let gone = fixture.path().join("vanished");
        let mut scan = Scan {
            failures: vec![WalkFailure {
                path: Some(gone.clone()),
                message: "Interrupted system call (os error 4)".to_owned(),
                transient: true,
                inaccessible: false,
            }],
            ..Scan::default()
        };

        retry_interrupted(fixture.path(), &classifier(), &verbose(), &mut scan, 0);

        assert_eq!(scan.failures.len(), 1);
        assert_eq!(scan.failures[0].path.as_deref(), Some(gone.as_path()));
    }
}
