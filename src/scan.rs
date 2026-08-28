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

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState};

use crate::caches::CacheRoots;
use crate::classify::{Classifier, Finding, Provenance, SkipReason, Verdict, is_dependency_dir};
use crate::error::{Error, Result};

/// Directories that are never descended into and never classified, whatever else is true of
/// them.
///
/// A VCS directory is not an artifact and its contents are irreplaceable, so it is out of reach
/// of every flag — `--clean-dependencies` included. Dependency caches are pruned just as hard
/// but are *not* in this list: they are classified at the directory itself before the prune, so
/// that the opt-in artifacts declared for them (ADR 0001's amendment) can fire.
pub const NEVER_DESCEND: &[&str] = &[".git", ".hg", ".svn"];

/// A compiled set of configured path globs, and the pattern each match came from.
///
/// Used for both `exclude` and `include` (ADR 0004). Both are evaluated during the walk rather
/// than against its results: an excluded directory is never descended into, and an included one
/// is taken whole, so neither costs anything below itself.
#[derive(Debug, Default)]
pub struct PatternSet {
    patterns: Vec<String>,
    globs: Option<GlobSet>,
}

impl PatternSet {
    /// Compiles the globs once, at configuration load, never per candidate.
    ///
    /// # Errors
    ///
    /// [`Error::CatalogPattern`] if a pattern will not compile.
    pub fn new(patterns: impl IntoIterator<Item = String>) -> Result<Self> {
        let patterns: Vec<String> = patterns.into_iter().collect();
        if patterns.is_empty() {
            return Ok(Self { patterns, globs: None });
        }
        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            let glob = Glob::new(pattern).map_err(|source| Error::CatalogPattern {
                pattern: pattern.clone(),
                source,
            })?;
            builder.add(glob);
        }
        let globs = builder.build().map_err(|source| Error::CatalogPattern {
            pattern: "<exclude set>".to_owned(),
            source,
        })?;
        Ok(Self {
            patterns,
            globs: Some(globs),
        })
    }

    /// The pattern that excludes this path, if any.
    #[must_use]
    pub fn matched(&self, path: &Path) -> Option<&str> {
        let globs = self.globs.as_ref()?;
        globs
            .matches(path)
            .first()
            .and_then(|index| self.patterns.get(*index))
            .map(String::as_str)
    }

    /// Whether anything is excluded at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.globs.is_none()
    }
}

/// How to walk.
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
}

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
    let (sender, receiver) = mpsc::channel();
    let visitor = Visitor {
        root,
        classifier,
        options,
    };

    configure(&mut WalkBuilder::new(root), options)
        .build_parallel()
        .run(|| {
            let sender = sender.clone();
            Box::new(move |result| visitor.visit(result, &sender))
        });

    drop(sender);
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
    if let Some(jobs) = options.jobs {
        builder.threads(jobs.max(1));
    }
    builder
}

/// Classifies one entry per call, from whichever worker thread produced it.
#[derive(Clone, Copy)]
struct Visitor<'a> {
    root: &'a Path,
    classifier: &'a Classifier,
    options: &'a ScanOptions,
}

impl Visitor<'_> {
    fn visit(
        &self,
        result: std::result::Result<ignore::DirEntry, ignore::Error>,
        sender: &mpsc::Sender<Message>,
    ) -> WalkState {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                let path = match &error {
                    ignore::Error::WithPath { path, .. } => Some(path.clone()),
                    _ => None,
                };
                // `EINTR` means the read was interrupted by a signal, not that the directory
                // could not be read. The walker's callback gives no way to resume the entry, so
                // it cannot be retried from here — but it can be told apart from a real failure
                // instead of being reported as one.
                let transient = error
                    .io_error()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::Interrupted);
                let failure = WalkFailure {
                    path,
                    message: error.to_string(),
                    transient,
                };
                // Sends cannot fail here: `scan_root` owns the `Receiver` and holds it until
                // `WalkBuilder::run` returns, which is after every worker has finished. The
                // same is true of the other sends in this file.
                let _ = sender.send(Message::Failure(Box::new(failure)));
                return WalkState::Continue;
            }
        };

        // Depth 0 is the scan root itself, which is never a candidate for its own removal.
        if entry.depth() == 0 {
            return WalkState::Continue;
        }

        let path = entry.path();
        // `file_type()` is what the walker already knows; a `metadata()` call here would cost
        // an extra syscall on every entry in the tree.
        let file_type = entry.file_type();
        let is_dir = file_type.is_some_and(|file_type| file_type.is_dir());
        let is_symlink = file_type.is_some_and(|file_type| file_type.is_symlink());

        if let Some(pattern) = self.options.exclude.matched(path) {
            self.note_skip(
                sender,
                path,
                SkipReason::Excluded {
                    pattern: pattern.to_owned(),
                },
            );
            return WalkState::Skip;
        }

        // `include` outranks every prune below but never `exclude`, which ADR 0004 makes
        // absolute. That ordering lets an explicit include *name* a directory the walk would
        // otherwise prune — a dependency directory or a tool cache, both of which ADR 0001
        // keeps out of the catalog and says configuration is the one way to reach.
        //
        // It does not let one reach *inside* such a directory: pruning is what makes the walk
        // fast, and once the walk declines to descend nothing below is offered to any matcher.
        // `.git` stays unreachable either way — the deletion guard refuses a VCS directory by
        // name whatever produced it.
        if let Some(pattern) = self.options.include.matched(path) {
            let finding = Finding {
                path: path.to_path_buf(),
                provenance: Provenance::Included {
                    pattern: pattern.to_owned(),
                },
            };
            let _ = sender.send(Message::Found(Box::new(finding)));
            return WalkState::Skip;
        }

        // After `exclude`, which ADR 0004 makes absolute, and before anything that could
        // classify: an installed toolchain is shaped exactly like a project, so the classifier
        // would prove it correctly and take it (ADR 0001).
        if is_dir && self.options.caches.contains(path) {
            self.note_skip(sender, path, SkipReason::ToolCache);
            return WalkState::Skip;
        }

        // Git housekeeping discovers repositories here, at the moment the walker meets `.git`
        // and is about to prune it: one name comparison, no syscall (ADR 0011). A second walk
        // over `$HOME` to find the same directories would roughly double the run, which is the
        // whole reason this is a line in the artifact walk rather than a pass of its own.
        //
        // It does not return: the prune below still refuses to descend into a `.git` directory,
        // and a `.git` *file* — a linked worktree or a submodule — falls through to the
        // classifier, which has nothing to say about it.
        if self.options.collect_repositories
            && let Some(work_tree) = crate::git::repository_at(entry.file_name(), path, is_symlink)
        {
            let _ = sender.send(Message::Repository(work_tree.to_path_buf()));
        }

        if is_dir && is_never_descended(entry.file_name()) {
            return WalkState::Skip;
        }

        if is_dir && is_dependency_dir(entry.file_name()) {
            self.visit_dependency_dir(entry.file_name(), path, sender);
            return WalkState::Skip;
        }

        // A symlink is classified as though it were a directory so a symlinked `target/` is
        // *seen* rather than silently invisible. It can never become a deletion — the deletion
        // guard refuses to remove through a link (ADR 0006) — but it is reported as a finding
        // the guard then refuses, not folded into the anonymous skip count. A refusal names the
        // path in the artifacts block; a skip is a number unless `-v` was passed, and a
        // symlinked `target/` beside a real `Cargo.toml` is exactly the thing a reader wants
        // told about.
        match self.classifier.classify(self.root, path, is_dir || is_symlink) {
            Verdict::Artifact(finding) => {
                let _ = sender.send(Message::Found(Box::new(finding)));
                // A matched artifact is an indivisible unit: it is sized and removed wholesale,
                // so walking inside it is wasted work and would produce nested findings that
                // make the report unreadable. A symlink is not descended for the older reason:
                // the walker does not follow links at all.
                if is_dir || is_symlink {
                    return WalkState::Skip;
                }
            }
            Verdict::Skip(reason) => self.note_skip(sender, path, reason),
            Verdict::NotACandidate => {}
        }

        WalkState::Continue
    }

    /// Decides what a dependency directory is, without ever entering it.
    ///
    /// ~keep The caller prunes unconditionally on return, and that is not negotiable: declining
    /// to enumerate `node_modules/` is where essentially all of the walk's wall-clock saving
    /// comes from. What this adds is one classification of the *directory itself*, which costs
    /// at most a single memoized `read_dir` of its anchor — a directory the classifier has
    /// almost always listed already for the artifacts beside it — and never touches an entry
    /// below. `should_not_enumerate_inside_an_enabled_dependency_directory` pins both halves.
    ///
    /// Classification runs whether or not the artifact is enabled, because the enable decision
    /// belongs to the classifier and to the configuration resolved at the artifact's own
    /// directory (ADR 0004) — the walker does not get a second opinion on it. With the opt-in
    /// off, the verdict is a `NotEnabled` skip that says which spec would turn it on.
    fn visit_dependency_dir(&self, name: &OsStr, path: &Path, sender: &mpsc::Sender<Message>) {
        // The inside-check runs on every verdict, including `Artifact`. The classifier here is
        // permissive (`run.rs` builds it that way so a deeper `voom.toml` can still turn an
        // artifact on), so an `Artifact` verdict means "this could be taken", not "this will
        // be" — and skipping the check on it lost Ruby's `vendor/bundle/` on any tree that also
        // had a `composer.json` beside the same `vendor/`. Whether the outer directory is
        // actually taken is decided in `run.rs`, which also drops a finding the outer one
        // covers, so nothing is counted twice.
        self.check_inside_dependency(name, path, sender);
        match self.classifier.classify(self.root, path, true) {
            Verdict::Artifact(finding) => {
                let _ = sender.send(Message::Found(Box::new(finding)));
            }
            Verdict::Skip(reason) => self.note_skip(sender, path, reason),
            Verdict::NotACandidate => {}
        }
    }

    /// Classifies the specific paths the catalog declares inside a dependency directory.
    ///
    /// Nothing else below is walked: this constructs the declared paths directly and then the
    /// caller prunes, so a `node_modules/` with a hundred thousand entries still costs nothing.
    fn check_inside_dependency(&self, name: &OsStr, dir: &Path, sender: &mpsc::Sender<Message>) {
        for id in self.classifier.artifacts_inside(name) {
            let artifact = id.artifact();
            // The first segment is the dependency directory itself, which is `dir`.
            let relative: PathBuf = artifact.segments().skip(1).collect();
            let candidate = dir.join(relative);
            let Ok(metadata) = std::fs::symlink_metadata(&candidate) else {
                continue;
            };
            // Same rule as the walk above: a symlinked artifact is reported and then refused,
            // never quietly counted.
            let is_dir = metadata.is_dir() || metadata.file_type().is_symlink();
            match self.classifier.classify(self.root, &candidate, is_dir) {
                Verdict::Artifact(finding) => {
                    let _ = sender.send(Message::Found(Box::new(finding)));
                }
                Verdict::Skip(reason) => self.note_skip(sender, &candidate, reason),
                Verdict::NotACandidate => {}
            }
        }
    }

    /// Records a passed-over candidate, keeping only the count unless detail was asked for.
    fn note_skip(&self, sender: &mpsc::Sender<Message>, path: &Path, reason: SkipReason) {
        let message = if self.options.collect_skips {
            Message::Skipped(Box::new(Skipped {
                path: path.to_path_buf(),
                reason,
            }))
        } else {
            Message::SkippedCount
        };
        let _ = sender.send(message);
    }
}

/// Whether a directory is one voom neither enters nor classifies.
fn is_never_descended(name: &OsStr) -> bool {
    NEVER_DESCEND.iter().any(|never| name == OsStr::new(never))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{ArtifactId, Selection};
    use crate::testing::tree;

    fn classifier() -> Classifier {
        Classifier::new(Selection::all()).expect("the catalog compiles")
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
    /// wants a path inside a cache swept names the cache as a scan root, or passes `--caches`.
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

        assert!(scan.findings.is_empty());
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

    /// The same tree with `--caches`, which is the whole point of the flag.
    #[test]
    fn should_descend_into_a_tool_cache_when_asked_to() {
        let fixture = tree(&[
            ".npm/_cacache/pkg/package.json",
            ".npm/_cacache/pkg/dist/bundle.js",
            "project/package.json",
            "project/dist/bundle.js",
        ]);
        let options = ScanOptions {
            caches: CacheRoots::for_root(fixture.path(), true),
            ..verbose()
        };

        let scan = run(fixture.path(), &options);

        assert_eq!(
            found(&scan, fixture.path()),
            vec![".npm/_cacache/pkg/dist", "project/dist"]
        );
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
        assert!(found(&scan, fixture.path()).is_empty());
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
        let scan = run(fixture.path(), &ScanOptions::default());
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

        assert!(found(&scan, fixture.path()).is_empty());
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
        assert!(scan.findings.is_empty());
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
}
