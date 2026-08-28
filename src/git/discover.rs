//! Finding repositories, and proving what each one is.
//!
//! Three questions live here, and they are the ones the safety argument rests on:
//!
//! 1. **Where are the repositories?** A sweep answers this for free — the artifact walker meets
//!    `.git` and prunes it, so [`repository_at`] collects the path at that point. The subcommand
//!    walks for itself, ignore-blind for the same reason `scan::configure` is.
//! 2. **Which repository is this, really?** A `.git` file is a linked worktree, a submodule, or a
//!    `--separate-git-dir` checkout, and telling them apart is what keeps voom from running
//!    housekeeping four times over one object store — or from folding a submodule into its
//!    superproject and never pruning it at all.
//! 3. **May voom touch it?** The protected denylist and containment, and whether a git operation
//!    is half-finished inside.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use ignore::{WalkBuilder, WalkState};

use super::{DOT_GIT, GitPruneOptions, Skipped};
use crate::caches::CacheRoots;
use crate::classify::is_dependency_dir;
use crate::delete::PROTECTED_PATHS;
use crate::scan::NEVER_DESCEND;

/// Files and directories inside a git directory that mean an operation is half-finished.
///
/// Running `gc` under somebody's paused rebase is the avoidable harm here: these are exactly the
/// states where the user still intends to `--continue`, and where the objects holding their work
/// are reachable only from files git treats as scratch.
const IN_PROGRESS_MARKERS: &[&str] = &[
    "rebase-merge",
    "rebase-apply",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
];

/// The working tree of the repository a walker entry proves, if that entry is a `.git`.
///
/// **This is how a sweep discovers repositories.** The artifact walker already has the entry in
/// hand — `.git` is in [`NEVER_DESCEND`], so it is examined and then pruned — and collecting the
/// path at that point costs one name comparison and no syscall. A second full walk over `$HOME`
/// to find the same directories would roughly double voom's headline number, which is the one
/// thing `perf-discipline` will not trade.
///
/// Both shapes count. A `.git` directory is a main repository; a `.git` *file* is a linked
/// worktree, a submodule, or a `--separate-git-dir` checkout, and [`resolve_all`] decides which.
/// A symlinked `.git` is not followed, for the same reason nothing else in voom is.
#[must_use]
pub fn repository_at<'a>(name: &OsStr, path: &'a Path, is_symlink: bool) -> Option<&'a Path> {
    if is_symlink || name != OsStr::new(DOT_GIT) {
        return None;
    }
    path.parent()
}

/// The standalone discovery walk, for the subcommand.
///
/// Configured ignore-blind for the same reason `scan::configure` is
/// (`adrs/0005-traversal-and-parallelism.md`): a repository very often sits inside a directory
/// some `.gitignore` mentions, and a walker that read ignore files would find a fraction of them.
pub(super) fn repositories(options: &GitPruneOptions) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in &options.roots {
        walk(root, options, &mut found);
    }
    found
}

fn walk(root: &Path, options: &GitPruneOptions, into: &mut Vec<PathBuf>) {
    let caches = CacheRoots::for_root(root, options.caches);
    // Shared by reference: every worker asks the same set, and `&CacheRoots` is `Copy`, which is
    // what lets the per-thread visitor be a `move` closure without cloning the set per thread.
    let caches = &caches;
    let (sender, receiver) = std::sync::mpsc::channel();

    let mut builder = WalkBuilder::new(root);
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

    builder.build_parallel().run(|| {
        let sender = sender.clone();
        Box::new(move |result| {
            let Ok(entry) = result else {
                // A directory that cannot be read holds no repository voom can act on, and here
                // the walk is a means rather than the subject — unlike a sweep, where a
                // permission failure is a finding the report owes the reader.
                return WalkState::Continue;
            };
            let path = entry.path();
            let file_type = entry.file_type();
            let is_symlink = file_type.is_some_and(|file_type| file_type.is_symlink());

            if let Some(work_tree) = repository_at(entry.file_name(), path, is_symlink) {
                let _ = sender.send(work_tree.to_path_buf());
                // A repository inside a repository is a submodule, and is reached through its
                // own `.git` file rather than by walking the superproject's object store.
                return WalkState::Skip;
            }
            if !file_type.is_some_and(|file_type| file_type.is_dir()) {
                return WalkState::Continue;
            }
            if options.exclude.matched(path).is_some() || caches.contains(path) || is_pruned(entry.file_name()) {
                return WalkState::Skip;
            }
            WalkState::Continue
        })
    });

    drop(sender);
    into.extend(receiver);
}

/// Whether a directory is one the discovery walk never enters.
fn is_pruned(name: &OsStr) -> bool {
    NEVER_DESCEND.iter().any(|never| name == OsStr::new(never)) || is_dependency_dir(name)
}

/// A repository resolved to the form git will be run against.
///
/// The fields are private and read through accessors so nothing outside this module can assemble
/// one: a `Located` is the claim that a path passed the denylist and containment, and a struct
/// literal elsewhere would be that claim made without the checks. Same reason `delete::Verified`
/// can only be minted by `Guard::check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Located {
    work_tree: PathBuf,
    common_dir: PathBuf,
    linked: bool,
}

impl Located {
    /// The working tree git runs in, canonicalized.
    pub(super) fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    /// The repository's common directory — its object store, and the deduplication key.
    pub(super) fn common_dir(&self) -> &Path {
        &self.common_dir
    }
}

#[derive(Debug, Clone)]
pub(super) enum Resolution {
    /// The repository to act on.
    Repository(Located),
    /// A path to report and not act on.
    Refused(PathBuf, Skipped),
}

impl Resolution {
    fn path(&self) -> &Path {
        match self {
            Self::Repository(located) => &located.work_tree,
            Self::Refused(path, _) => path,
        }
    }
}

/// Resolves every discovered path and keeps one entry per repository.
///
/// Deduplication is on the **common directory**, not on the working tree, because that is what
/// makes the linked-worktree rule true: four worktrees of one repository share one object store,
/// and running `gc` in each would run it four times over the same objects. A submodule has a
/// common directory of its own, so it survives deduplication and is pruned in its own right.
pub(super) fn resolve_all(work_trees: &[PathBuf], roots: &[PathBuf]) -> Vec<Resolution> {
    let protected = resolved_protected_paths();
    let mut resolved: Vec<Resolution> = work_trees
        .iter()
        .map(|work_tree| resolve(work_tree, roots, &protected))
        .collect();

    // A main repository sorts before a linked worktree of the same repository, so it is the one
    // kept — the report then names the checkout the user thinks of as "the repository".
    resolved.sort_by(|left, right| match (left, right) {
        (Resolution::Repository(first), Resolution::Repository(second)) => first
            .linked
            .cmp(&second.linked)
            .then_with(|| first.work_tree.cmp(&second.work_tree)),
        _ => left.path().cmp(right.path()),
    });

    let mut seen = BTreeSet::new();
    resolved.retain(|resolution| match resolution {
        Resolution::Repository(located) => seen.insert(located.common_dir.clone()),
        Resolution::Refused(path, _) => seen.insert(path.clone()),
    });
    resolved
}

fn resolve(work_tree: &Path, roots: &[PathBuf], protected: &[PathBuf]) -> Resolution {
    let located = match locate(work_tree) {
        Ok(located) => located,
        Err(Some(detail)) => {
            return Resolution::Refused(canonical_or_given(work_tree), Skipped::Unresolved { detail });
        }
        Err(None) => return Resolution::Refused(canonical_or_given(work_tree), Skipped::Vanished),
    };

    if is_protected(&located.work_tree, protected) {
        return Resolution::Refused(located.work_tree, Skipped::Protected);
    }
    if !contained(&located.work_tree, roots) {
        return Resolution::Refused(located.work_tree, Skipped::EscapesRoot);
    }
    Resolution::Repository(located)
}

/// Works out which repository a `.git` entry belongs to, and where its object store is.
///
/// Three shapes reach here, and telling them apart is what keeps the linked-worktree rule from
/// swallowing submodules:
///
/// - a `.git` **directory** — a main repository, object store beneath it;
/// - a `.git` **file** whose gitdir sits under `<main>/.git/worktrees/<name>` — a linked worktree,
///   which shares the main repository's object store and must not be pruned in its own right;
/// - any other `.git` file — a submodule (`<super>/.git/modules/<name>`) or a
///   `--separate-git-dir` checkout, both of which have an object store of their own and are
///   repositories in their own right.
///
/// `Err(Some(detail))` is a shape voom will not guess at; `Err(None)` is "not a repository".
fn locate(work_tree: &Path) -> Result<Located, Option<String>> {
    let dot_git = work_tree.join(DOT_GIT);
    let metadata = std::fs::symlink_metadata(&dot_git).map_err(|_| None)?;
    let canonical = work_tree.canonicalize().map_err(|_| None)?;

    if metadata.file_type().is_dir() {
        return Ok(Located {
            common_dir: canonical.join(DOT_GIT),
            work_tree: canonical,
            linked: false,
        });
    }
    if !metadata.file_type().is_file() {
        // A symlinked `.git` is not followed, for the same reason nothing else in voom is.
        return Err(None);
    }

    let git_dir = read_gitdir(&dot_git, work_tree).map_err(Some)?;
    match main_work_tree(&git_dir) {
        // A linked worktree, resolved to the repository it belongs to.
        Some((common_dir, main)) => Ok(Located {
            work_tree: main,
            common_dir,
            linked: true,
        }),
        // A submodule or a separate git directory: its own repository, run in place.
        None => Ok(Located {
            common_dir: git_dir,
            work_tree: canonical,
            linked: true,
        }),
    }
}

fn read_gitdir(dot_git: &Path, work_tree: &Path) -> Result<PathBuf, String> {
    let contents = std::fs::read_to_string(dot_git).map_err(|error| error.to_string())?;
    let gitdir = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))
        .ok_or_else(|| "its `.git` file names no gitdir".to_owned())?
        .trim();
    let absolute = if Path::new(gitdir).is_absolute() {
        PathBuf::from(gitdir)
    } else {
        work_tree.join(gitdir)
    };
    absolute
        .canonicalize()
        .map_err(|error| format!("its gitdir `{gitdir}` does not resolve: {error}"))
}

/// The common directory and main working tree, when `git_dir` is a linked worktree's private
/// directory — `<main>/.git/worktrees/<name>`.
fn main_work_tree(git_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let worktrees = git_dir
        .ancestors()
        .find(|ancestor| ancestor.file_name() == Some(OsStr::new("worktrees")))?;
    let common = worktrees.parent()?;
    if common.file_name() != Some(OsStr::new(DOT_GIT)) {
        return None;
    }
    Some((common.to_path_buf(), common.parent()?.to_path_buf()))
}

fn canonical_or_given(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Every protected path, resolved once per run against this machine.
///
/// `PROTECTED_PATHS` is the sweep's append-only denylist (ADR 0006), reused here rather than
/// restated. The literals are checked as written *and* canonicalized, because on macOS `/tmp`
/// canonicalizes to `/private/tmp` and a comparison against one form alone is a rail with a hole
/// in it.
fn resolved_protected_paths() -> Vec<PathBuf> {
    PROTECTED_PATHS
        .iter()
        .filter_map(|protected| Path::new(protected).canonicalize().ok())
        // The home directory, as the deletion guard also does. A machine with dotfiles versioned
        // at `$HOME` has a repository there, and `voom ~` would otherwise run housekeeping in it
        // by default. Nothing here deletes anything, so this is a belt-and-braces rail rather
        // than a load-bearing one — but the two lists are described as the same denylist, and a
        // reader who checks one and not the other must not get two answers.
        .chain(dirs::home_dir().and_then(|home| home.canonicalize().ok()))
        .collect()
}

fn is_protected(canonical: &Path, resolved: &[PathBuf]) -> bool {
    canonical.parent().is_none()
        || PROTECTED_PATHS
            .iter()
            .any(|protected| canonical == Path::new(protected))
        || resolved.iter().any(|protected| canonical == protected.as_path())
}

/// Whether the repository sits at or below one of the scan roots.
///
/// The sweep's containment rail demands *strictly* below, because there the path in question is
/// about to be deleted and voom removes things *in* the tree it was pointed at, never the tree
/// itself. Nothing is deleted here — git decides what is prunable and does the removing — so the
/// equality case is the ordinary invocation, `voom git-prune .` inside a repository, rather than a
/// hazard. Refusing it would leave the command unable to do the most obvious thing anyone will ask
/// of it.
fn contained(canonical: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| canonical.starts_with(root))
}

/// The path of the marker proving a git operation is half-finished here, if there is one.
///
/// Both the common directory and each linked worktree's private directory are checked: a rebase
/// paused in a worktree lives in `.git/worktrees/<name>/rebase-merge`, and a repack of the shared
/// object store is the same hazard wherever the paused operation is.
pub(super) fn in_progress(common_dir: &Path) -> Option<PathBuf> {
    if let Some(marker) = marker_in(common_dir) {
        return Some(common_dir.join(marker));
    }
    let entries = std::fs::read_dir(common_dir.join("worktrees")).ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if let Some(marker) = marker_in(&dir) {
            return Some(dir.join(marker));
        }
    }
    None
}

fn marker_in(dir: &Path) -> Option<&'static str> {
    IN_PROGRESS_MARKERS
        .iter()
        .find(|marker| dir.join(marker).exists())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tree;

    /// The walker surface a sweep collects repositories through: one name comparison, no syscall.
    #[test]
    fn should_recognise_a_git_entry_as_its_parents_repository() {
        let path = Path::new("/projects/api/.git");
        assert_eq!(
            repository_at(OsStr::new(".git"), path, false),
            Some(Path::new("/projects/api"))
        );
        assert_eq!(
            repository_at(OsStr::new("target"), Path::new("/projects/api/target"), false),
            None
        );
        assert_eq!(
            repository_at(OsStr::new(".git"), path, true),
            None,
            "a symlinked .git is not followed"
        );
    }

    #[test]
    fn should_resolve_a_linked_worktree_to_its_main_repository() {
        assert_eq!(
            main_work_tree(Path::new("/projects/api/.git/worktrees/feature")),
            Some((PathBuf::from("/projects/api/.git"), PathBuf::from("/projects/api")))
        );
    }

    /// A submodule's `.git` file points into `<super>/.git/modules/<name>`, which is an object
    /// store of its own — so it is a repository in its own right and must survive the
    /// deduplication that folds linked worktrees into their main repository.
    #[test]
    fn should_not_treat_a_submodule_as_a_linked_worktree() {
        assert_eq!(main_work_tree(Path::new("/projects/api/.git/modules/vendor")), None);
    }

    /// A `.git` file naming a shape voom does not recognise resolves to the checkout itself
    /// rather than to a guess. Guessing is how the wrong repository gets repacked.
    #[test]
    fn should_locate_a_separate_git_dir_checkout_as_its_own_repository() {
        let fixture = tree(&["work/keep.txt", "store/HEAD"]);
        std::fs::write(
            fixture.path().join("work/.git"),
            format!("gitdir: {}\n", fixture.path().join("store").display()),
        )
        .expect("the .git file");

        let located = locate(&fixture.path().join("work")).expect("a repository");

        assert_eq!(
            located.common_dir(),
            fixture.path().join("store").canonicalize().expect("it exists")
        );
        assert_eq!(
            located.work_tree(),
            fixture.path().join("work").canonicalize().expect("it exists")
        );
    }

    /// The rail that keeps a repack out of somebody's paused rebase.
    #[test]
    fn should_find_an_in_progress_marker_in_the_git_directory() {
        let fixture = tree(&[".git/rebase-merge/head-name"]);
        let git_dir = fixture.path().join(".git");
        assert_eq!(in_progress(&git_dir), Some(git_dir.join("rebase-merge")));
    }

    /// A rebase paused inside a linked worktree is the same hazard: the objects holding the
    /// user's work live in the main repository's shared object store.
    #[test]
    fn should_find_an_in_progress_marker_in_a_linked_worktree() {
        let fixture = tree(&[".git/worktrees/feature/MERGE_HEAD"]);
        let git_dir = fixture.path().join(".git");
        assert_eq!(
            in_progress(&git_dir),
            Some(git_dir.join("worktrees/feature/MERGE_HEAD"))
        );
    }

    #[test]
    fn should_find_no_marker_in_a_quiet_repository() {
        let fixture = tree(&[".git/HEAD"]);
        assert_eq!(in_progress(&fixture.path().join(".git")), None);
    }

    /// The rail is reused from the sweep, so `/` and the rest of `PROTECTED_PATHS` are refused
    /// here too.
    #[test]
    fn should_refuse_a_protected_path() {
        let protected = resolved_protected_paths();
        assert!(is_protected(Path::new("/"), &protected));
        assert!(!is_protected(Path::new("/definitely/not/a/protected/path"), &protected));
    }

    /// Containment allows the root itself, which the sweep's rail does not — `voom git-prune .`
    /// inside a repository is the ordinary invocation, and nothing here is deleted.
    #[test]
    fn should_contain_a_repository_at_or_below_a_root() {
        let roots = vec![PathBuf::from("/projects")];
        assert!(contained(Path::new("/projects"), &roots));
        assert!(contained(Path::new("/projects/api"), &roots));
        assert!(!contained(Path::new("/elsewhere/api"), &roots));
    }

    /// A repository outside every scan root is named and left alone rather than acted on.
    #[test]
    fn should_refuse_a_repository_outside_every_root() {
        let fixture = tree(&[".git/HEAD"]);
        let elsewhere = tree(&["unrelated/"]);

        let resolved = resolve_all(
            &[fixture.path().to_path_buf()],
            &[elsewhere.path().canonicalize().expect("it exists")],
        );

        assert!(
            matches!(&resolved[..], [Resolution::Refused(_, Skipped::EscapesRoot)]),
            "{resolved:?}"
        );
    }
}
