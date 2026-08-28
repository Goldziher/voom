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

/// Directories that are never descended into, whatever else is true of them.
///
/// `.git` is not an artifact and its contents are irreplaceable; the rest are the dependency
/// caches ADR 0001 puts out of scope. Skipping them is where most of the saving comes from —
/// they are the deepest and widest subtrees on a typical disk.
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
}

enum Message {
    Found(Box<Finding>),
    Skipped(Box<Skipped>),
    SkippedCount,
    Failure(Box<WalkFailure>),
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
        // absolute. That ordering is what lets an explicit include reach into a dependency
        // directory — ADR 0001 keeps those out of the catalog, and says configuration is the
        // one way to name them anyway. `.git` stays unreachable regardless: the deletion guard
        // refuses a VCS directory by name whatever produced it.
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

        if is_dir && is_pruned(entry.file_name()) {
            // Two catalog entries declare build output *inside* a dependency directory
            // (Ruby's `vendor/bundle/`, Julia's `deps/build/`). Check those exact paths
            // before pruning, so the entries are reachable without walking the cache below.
            self.check_inside_dependency(entry.file_name(), path, sender);
            return WalkState::Skip;
        }

        // A symlink is classified as though it were a directory so a symlinked `target/` is
        // *seen* and reported rather than silently invisible — but it can never become a
        // deletion, because ADR 0006 refuses to delete through one.
        match self.classifier.classify(self.root, path, is_dir || is_symlink) {
            Verdict::Artifact(finding) => {
                if is_symlink {
                    self.note_skip(sender, path, SkipReason::Symlink);
                    return WalkState::Skip;
                }
                let _ = sender.send(Message::Found(Box::new(finding)));
                // A matched artifact is an indivisible unit: it is sized and removed wholesale,
                // so walking inside it is wasted work and would produce nested findings that
                // make the report unreadable.
                if is_dir {
                    return WalkState::Skip;
                }
            }
            Verdict::Skip(reason) => self.note_skip(sender, path, reason),
            Verdict::NotACandidate => {}
        }

        WalkState::Continue
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
            if metadata.file_type().is_symlink() {
                self.note_skip(sender, &candidate, SkipReason::Symlink);
                continue;
            }
            match self.classifier.classify(self.root, &candidate, metadata.is_dir()) {
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

/// Whether a directory is one voom never enters.
fn is_pruned(name: &std::ffi::OsStr) -> bool {
    NEVER_DESCEND.iter().any(|never| name == std::ffi::OsStr::new(never)) || is_dependency_dir(name)
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
    /// source tree. It is seen and reported, which is more useful than being invisible.
    #[test]
    #[cfg(unix)]
    fn should_report_a_symlinked_artifact_as_skipped_rather_than_found() {
        let fixture = tree(&["Cargo.toml", "elsewhere/precious.txt"]);
        std::os::unix::fs::symlink(fixture.path().join("elsewhere"), fixture.path().join("target")).expect("a symlink");

        let scan = run(fixture.path(), &verbose());
        assert!(found(&scan, fixture.path()).is_empty(), "a symlink is never a finding");
        assert!(scan.skips.iter().any(|skip| skip.reason == SkipReason::Symlink));
        assert!(fixture.path().join("elsewhere/precious.txt").exists());
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
