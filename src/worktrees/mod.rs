//! Merged-worktree cleanup — linked worktrees whose work is already in the default branch.
//!
//! A worktree checked out for one ticket, merged, and never removed costs a full checkout and
//! (through its Bazel output base, see [`crate::bazel`]) several gigabytes more. Unlike the
//! housekeeping in [`crate::git`], removing one can lose work, so nothing here runs unless asked
//! for by name, and "merged" is never the whole test. A worktree is removed only when **all** of
//! these hold:
//!
//! - its `HEAD` is an ancestor of the repository's default branch, so every commit is reachable
//!   from somewhere else;
//! - its working tree holds nothing but deletions of tracked build output (a `dist/` that was
//!   cleaned, at any depth; `build/` and `out/` only at the top level, where they are not source)
//!   and ignored build-output trees (`target/`, `node_modules/`) anywhere; any modified, staged,
//!   added, renamed or untracked path, any ignored file that is not build output, and a deleted
//!   tracked file under a nested `src/build/`, keep it, reported as merged-with-local-changes;
//! - it is not the main worktree, not locked, not missing, not where voom was started, and
//!   stored strictly below a path voom was told to sweep — a worktree elsewhere on disk is
//!   reported and kept, so nothing outside the scanned tree is ever its target;
//!
//! The default branch is read from the local remote-tracking ref and voom never fetches, so a
//! stale ref errs toward keeping. `git worktree remove` does the removal and the branch is left
//! alone: deleting a branch is a separate decision this module never makes.

mod report;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rayon::prelude::*;

pub use report::{document, render_human, render_json};

/// The JSON shape's version.
pub const SCHEMA_VERSION: u32 = 1;

/// Path components whose tracked deletions are build output, not work.
const BUILD_OUTPUT_DIRS: &[&str] = &["dist", "build", "target", "out", "node_modules", "__pycache__"];

/// The build-output names that are never source, so a tracked deletion under one is discardable at
/// any depth. `build` and `out` are left out: they are only build output at the top level.
const UNAMBIGUOUS_BUILD_OUTPUT_DIRS: &[&str] = &["dist", "target", "node_modules", "__pycache__"];

/// The default-branch candidates, most authoritative first. The first that resolves wins.
const DEFAULT_BRANCH_FALLBACKS: &[&str] = &[
    "refs/remotes/origin/main",
    "refs/remotes/origin/master",
    "refs/heads/main",
    "refs/heads/master",
];

/// What to do.
#[derive(Debug, Clone, Copy)]
pub struct WorktreeOptions {
    /// Report what would be removed without removing it.
    pub dry_run: bool,
    /// Also remove a worktree that is not merged but has been idle at least this long, provided
    /// its commits are reachable from another ref. `None` removes only merged worktrees.
    pub stale_after: Option<Duration>,
}

/// Why a worktree was removed or kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// The repository's own checkout; never a candidate.
    Main,
    /// `git worktree lock` was run on it.
    Locked,
    /// voom was started inside it.
    Current,
    /// Its directory is gone; `git worktree prune` owns that case.
    Missing,
    /// Its `HEAD` is not in the default branch.
    NotMerged,
    /// Its directory resolves outside every scan root. voom removes only what the walk covered,
    /// so a worktree stored elsewhere is reported and kept.
    OutsideRoot,
    /// Merged, but something in the working tree is not regenerable build output.
    LocalChanges {
        /// How many paths are changed, staged or untracked.
        changed: usize,
    },
    /// Merged and clean apart from deleted build output.
    Merged {
        /// How many deleted tracked build-output paths removal discards.
        discarded: usize,
    },
    /// Not merged, but idle past the stale age, its commits safe on another ref, and clean apart
    /// from build output.
    Stale {
        /// How many deleted tracked build-output paths removal discards.
        discarded: usize,
    },
    /// Git could not answer (status failed, ancestry check errored).
    Unchecked(String),
}

/// What happened to a [`State::Merged`] or [`State::Stale`] worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Removed.
    Removed,
    /// A dry run would remove it.
    WouldRemove,
    /// `git worktree remove` refused or failed.
    Failed(String),
}

/// One linked worktree.
#[derive(Debug, Clone)]
pub struct Worktree {
    /// Its directory.
    pub path: PathBuf,
    /// The branch checked out, if not detached.
    pub branch: Option<String>,
    /// What was proven.
    pub state: State,
    /// What was done, for a merged one.
    pub outcome: Option<Outcome>,
}

/// One repository's worktrees.
#[derive(Debug, Clone)]
pub struct RepositoryWorktrees {
    /// The repository's common git directory's work tree (its main checkout).
    pub repository: PathBuf,
    /// The ref merged-ness was judged against.
    pub default_branch: Option<String>,
    /// Every worktree but the main one, sorted by path.
    pub worktrees: Vec<Worktree>,
    /// Why the repository could not be examined, when it could not.
    pub error: Option<String>,
}

/// Everything one run produced.
#[derive(Debug)]
pub struct WorktreePruneResult {
    /// Per repository, sorted by path.
    pub repositories: Vec<RepositoryWorktrees>,
    /// Whether removal was withheld.
    pub dry_run: bool,
    /// Wall-clock time.
    pub elapsed: Duration,
}

/// The footer's numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Removed, or that would be.
    pub removed: usize,
    /// Merged but held back by local changes.
    pub local_changes: usize,
    /// Every other worktree left alone.
    pub kept: usize,
    /// Removals that failed.
    pub failed: usize,
}

impl WorktreePruneResult {
    /// The footer's numbers.
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals::default();
        for worktree in self.repositories.iter().flat_map(|repository| &repository.worktrees) {
            match (&worktree.state, &worktree.outcome) {
                (State::Merged { .. } | State::Stale { .. }, Some(Outcome::Removed | Outcome::WouldRemove)) => {
                    totals.removed += 1;
                }
                (State::Merged { .. } | State::Stale { .. }, Some(Outcome::Failed(_))) => totals.failed += 1,
                (State::LocalChanges { .. }, _) => totals.local_changes += 1,
                _ => totals.kept += 1,
            }
        }
        totals
    }

    /// Nonzero when a removal failed.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if self.totals().failed > 0 {
            crate::report::exit::REMOVAL_FAILED
        } else {
            crate::report::exit::SUCCESS
        }
    }
}

/// Examines the repositories owning the given work trees and removes their merged worktrees.
///
/// `work_trees` may name the main checkout or any linked one; repositories are deduplicated by
/// git's common directory, so a repository reached through three of its worktrees is handled
/// once. A path that is not a repository is ignored.
///
/// A worktree is removed only when its canonical path resolves strictly below one of `roots`:
/// the repositories are found by the walk, but a linked worktree can live anywhere on disk, and
/// voom removes nothing the tree it was told to sweep does not cover.
#[must_use]
pub fn prune(work_trees: &[PathBuf], roots: &[PathBuf], options: WorktreeOptions) -> WorktreePruneResult {
    let started = Instant::now();
    let current = std::env::current_dir().ok().and_then(|cwd| cwd.canonicalize().ok());
    let roots: Vec<PathBuf> = roots.iter().filter_map(|root| root.canonicalize().ok()).collect();

    let mut seen = std::collections::BTreeSet::new();
    let repositories: Vec<PathBuf> = work_trees
        .iter()
        .filter_map(|work_tree| {
            let common = git(work_tree, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
            seen.insert(PathBuf::from(common.trim())).then(|| work_tree.clone())
        })
        .collect();

    let mut found: Vec<RepositoryWorktrees> = repositories
        .par_iter()
        .map(|work_tree| examine(work_tree, &roots, current.as_deref(), options))
        .collect();
    found.retain(|repository| !repository.worktrees.is_empty() || repository.error.is_some());
    found.sort_by(|left, right| left.repository.cmp(&right.repository));

    WorktreePruneResult {
        repositories: found,
        dry_run: options.dry_run,
        elapsed: started.elapsed(),
    }
}

fn examine(
    work_tree: &Path,
    roots: &[PathBuf],
    current: Option<&Path>,
    options: WorktreeOptions,
) -> RepositoryWorktrees {
    let mut repository = RepositoryWorktrees {
        repository: work_tree.to_path_buf(),
        default_branch: None,
        worktrees: Vec::new(),
        error: None,
    };
    let Some(listing) = git(work_tree, &["worktree", "list", "--porcelain"]) else {
        repository.error = Some("`git worktree list` failed".to_owned());
        return repository;
    };
    let entries = parse_listing(&listing);
    // The first entry is the main checkout, which is the path to run git commands from.
    if let Some(main) = entries.first() {
        repository.repository.clone_from(&main.path);
    }
    let Some((default_ref, default_sha)) = default_branch(&repository.repository) else {
        repository.error = Some("no default branch found (origin/HEAD, main, master)".to_owned());
        return repository;
    };
    repository.default_branch = Some(default_ref);

    // Fanned out per worktree: ancestry is cheap and settles most of them, but a status over a
    // monorepo checkout with tens of thousands of changes is not, and one repository can own
    // a hundred worktrees. The removal itself is serialised per repository by `removal_lock`,
    // because concurrent `git worktree remove` runs mutate the same `worktrees/` bookkeeping.
    let removal_lock = std::sync::Mutex::new(());
    repository.worktrees = entries
        .par_iter()
        .skip(1)
        .filter(|entry| !entry.bare)
        .map(|entry| {
            judge(
                entry,
                &repository.repository,
                &default_sha,
                roots,
                current,
                options,
                &removal_lock,
            )
        })
        .collect();
    repository.worktrees.sort_by(|left, right| left.path.cmp(&right.path));
    repository
}

fn judge(
    entry: &Entry,
    main: &Path,
    default_sha: &str,
    roots: &[PathBuf],
    current: Option<&Path>,
    options: WorktreeOptions,
    removal_lock: &std::sync::Mutex<()>,
) -> Worktree {
    // git reports a worktree path in its own spelling; canonicalize it to the form the roots and
    // the rest of voom compare (on Windows that is the verbatim `\\?\` form). A path that no
    // longer exists — an already-removed worktree — falls back to what git said.
    let canonical = entry.path.canonicalize().unwrap_or_else(|_| entry.path.clone());
    let mut worktree = Worktree {
        path: canonical.clone(),
        branch: entry.branch.clone(),
        state: State::NotMerged,
        outcome: None,
    };
    if entry.locked {
        worktree.state = State::Locked;
        return worktree;
    }
    if !entry.path.is_dir() {
        worktree.state = State::Missing;
        return worktree;
    }
    if !roots
        .iter()
        .any(|root| canonical != *root && canonical.starts_with(root))
    {
        worktree.state = State::OutsideRoot;
        return worktree;
    }
    if current.is_some_and(|cwd| cwd.starts_with(&canonical)) {
        worktree.state = State::Current;
        return worktree;
    }
    let merged = match is_ancestor(main, &entry.head, default_sha) {
        Ok(merged) => merged,
        Err(message) => {
            worktree.state = State::Unchecked(message);
            return worktree;
        }
    };
    // Not merged is kept unless the caller named a stale age and the worktree is provably idle
    // past it with its commits safe on another ref.
    if !merged && !options.stale_after.is_some_and(|age| is_stale(entry, main, age)) {
        return worktree;
    }
    let Some(status) = git_bytes(
        &entry.path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            // Ignored files are invisible without this, and `git worktree remove` deletes them:
            // a worktree whose only content is a gitignored `.env` otherwise looks clean and is
            // removed. They are reported so a non-build-output one can keep the worktree.
            "--ignored=matching",
        ],
    ) else {
        worktree.state = State::Unchecked("`git status` failed".to_owned());
        return worktree;
    };
    let (discarded, changed) = classify_status(&status);
    if changed > 0 {
        worktree.state = State::LocalChanges { changed };
        return worktree;
    }
    worktree.state = if merged {
        State::Merged { discarded }
    } else {
        State::Stale { discarded }
    };
    worktree.outcome = Some(if options.dry_run {
        Outcome::WouldRemove
    } else {
        let _serialised = removal_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        remove(main, &entry.path, discarded > 0)
    });
    worktree
}

/// Splits `git status --porcelain=v1 -z` into (deleted build output, everything else).
fn classify_status(status: &[u8]) -> (usize, usize) {
    let mut discarded = 0;
    let mut changed = 0;
    let mut fields = status.split(|byte| *byte == 0).filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        if field.len() < 4 {
            changed += 1;
            continue;
        }
        let (index, tree) = (field[0], field[1]);
        let path = String::from_utf8_lossy(&field[3..]);
        // A rename or copy carries its source as the next field.
        if matches!(index, b'R' | b'C') || matches!(tree, b'R' | b'C') {
            fields.next();
        }
        if index == b' ' && tree == b'D' && is_deleted_build_output(&path) {
            discarded += 1;
        } else if index == b'!' && tree == b'!' && is_build_output(&path) {
            // An ignored build-output tree (target/, node_modules/) is regenerable and goes with
            // the worktree. Any other ignored file — a `.env`, a local database, notes — is not,
            // and keeps it.
            discarded += 1;
        } else {
            changed += 1;
        }
    }
    (discarded, changed)
}

/// Any path component naming build output. Used for an *ignored* tree, which is regenerable by
/// the user's own `.gitignore` declaration wherever it sits.
fn is_build_output(path: &str) -> bool {
    path.split('/').any(|part| BUILD_OUTPUT_DIRS.contains(&part))
}

/// Whether a deleted *tracked* file was build output. `dist/`, `target/`, `node_modules/` and
/// `__pycache__/` are build output wherever they sit — a monorepo commits `client/dist/` several
/// levels down, and a sweep deletes it. `build/` and `out/` are build output only at the top
/// level: deeper they are as often source (`src/build/`, `internal/out/`), and that deletion is a
/// choice removal would undo. The file's content survives in `HEAD` either way.
fn is_deleted_build_output(path: &str) -> bool {
    let mut parts = path.split('/');
    let top_level = parts.next();
    if top_level.is_some_and(|first| BUILD_OUTPUT_DIRS.contains(&first)) {
        return true;
    }
    // Everything after the file name is not a directory; the file itself is excluded.
    let mut directories: Vec<&str> = parts.collect();
    directories.pop();
    directories
        .iter()
        .any(|part| UNAMBIGUOUS_BUILD_OUTPUT_DIRS.contains(part))
}

fn remove(main: &Path, path: &Path, force: bool) -> Outcome {
    let mut command = base_command(main);
    command.args(["worktree", "remove"]);
    if force {
        command.arg("--force");
    }
    // `path` is the canonical worktree path; git does not accept the verbatim `\\?\` form that
    // `canonicalize` produces on Windows, so strip it back to a path git understands.
    command.arg(&*crate::git::for_git(path));
    match command.output() {
        Ok(output) if output.status.success() => Outcome::Removed,
        Ok(output) => Outcome::Failed(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("")
                .to_owned(),
        ),
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

fn is_ancestor(main: &Path, head: &str, default_sha: &str) -> Result<bool, String> {
    let output = base_command(main)
        .args(["merge-base", "--is-ancestor", head, default_sha])
        .output()
        .map_err(|error| error.to_string())?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err("`git merge-base` failed".to_owned()),
    }
}

/// Whether an unmerged worktree has gone quiet and removing it loses no commit.
///
/// Idle is the later of the `HEAD` commit's committer date and the modification time of the
/// worktree's `HEAD` reflog, which git rewrites on commit, checkout, rebase and reset but not on
/// `git status` (the index is rewritten by every status, so an IDE's background refresh would
/// make nothing ever stale). Removal keeps the branch, so the commits are safe when some ref
/// contains `HEAD` — a detached `HEAD` that no ref reaches would be lost, and is never stale.
fn is_stale(entry: &Entry, main: &Path, age: Duration) -> bool {
    let reachable = git(main, &["for-each-ref", "--count=1", "--contains", &entry.head, "refs/"])
        .is_some_and(|refs| !refs.trim().is_empty());
    if !reachable {
        return false;
    }
    let committed = git(&entry.path, &["log", "-1", "--format=%ct", "HEAD"])
        .and_then(|seconds| seconds.trim().parse::<u64>().ok())
        .map(|seconds| std::time::UNIX_EPOCH + Duration::from_secs(seconds));
    let reflog = git(&entry.path, &["rev-parse", "--path-format=absolute", "--git-dir"])
        .and_then(|dir| std::fs::metadata(Path::new(dir.trim()).join("logs/HEAD")).ok())
        .and_then(|metadata| metadata.modified().ok());
    // Without a commit date there is nothing to age; an unreadable reflog only means the
    // commit date decides.
    let Some(latest) = [committed, reflog].into_iter().flatten().max() else {
        return false;
    };
    committed.is_some() && latest.elapsed().is_ok_and(|idle| idle >= age)
}

/// The default branch as (ref name, commit). Never fetches.
fn default_branch(main: &Path) -> Option<(String, String)> {
    let symbolic = git(main, &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]);
    let candidates = symbolic
        .iter()
        .map(|target| target.trim().to_owned())
        .chain(DEFAULT_BRANCH_FALLBACKS.iter().map(|name| (*name).to_owned()));
    for candidate in candidates {
        if let Some(sha) = git(
            main,
            &["rev-parse", "--verify", "--quiet", &format!("{candidate}^{{commit}}")],
        ) {
            let short = candidate
                .trim_start_matches("refs/remotes/")
                .trim_start_matches("refs/heads/");
            return Some((short.to_owned(), sha.trim().to_owned()));
        }
    }
    None
}

#[derive(Debug, Default)]
struct Entry {
    path: PathBuf,
    head: String,
    branch: Option<String>,
    locked: bool,
    bare: bool,
}

fn parse_listing(listing: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    for block in listing.split("\n\n") {
        let mut entry = Entry::default();
        for line in block.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                entry.path = PathBuf::from(path);
            } else if let Some(head) = line.strip_prefix("HEAD ") {
                head.clone_into(&mut entry.head);
            } else if let Some(branch) = line.strip_prefix("branch ") {
                entry.branch = Some(branch.trim_start_matches("refs/heads/").to_owned());
            } else if line == "bare" {
                entry.bare = true;
            } else if line == "locked" || line.starts_with("locked ") {
                entry.locked = true;
            }
        }
        if !entry.path.as_os_str().is_empty() {
            entries.push(entry);
        }
    }
    entries
}

/// A git command that cannot run repository hooks or prompt, and cannot be made to run arbitrary
/// code from repository config: `core.fsmonitor` names a hook `git status` would otherwise
/// execute, and this code runs `status` in a checkout voom does not own.
fn base_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args(["-c", "core.hooksPath=", "-c", "core.fsmonitor=false"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    command
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    git_bytes(dir, args).and_then(|bytes| String::from_utf8(bytes).ok())
}

fn git_bytes(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = base_command(dir).args(args).output().ok()?;
    output.status.success().then_some(output.stdout)
}
