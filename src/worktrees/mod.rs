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
//!   cleaned), which is the only local change that is regenerable by definition — any modified,
//!   staged, added, renamed or untracked path keeps it, reported as merged-with-local-changes;
//! - it is not the main worktree, not locked, not missing, and not where voom was started.
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
    /// Git could not answer (status failed, ancestry check errored).
    Unchecked(String),
}

/// What happened to a [`State::Merged`] worktree.
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
                (State::Merged { .. }, Some(Outcome::Removed | Outcome::WouldRemove)) => totals.removed += 1,
                (State::Merged { .. }, Some(Outcome::Failed(_))) => totals.failed += 1,
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
#[must_use]
pub fn prune(work_trees: &[PathBuf], options: WorktreeOptions) -> WorktreePruneResult {
    let started = Instant::now();
    let current = std::env::current_dir().ok().and_then(|cwd| cwd.canonicalize().ok());

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
        .map(|work_tree| examine(work_tree, current.as_deref(), options))
        .collect();
    found.retain(|repository| !repository.worktrees.is_empty() || repository.error.is_some());
    found.sort_by(|left, right| left.repository.cmp(&right.repository));

    WorktreePruneResult {
        repositories: found,
        dry_run: options.dry_run,
        elapsed: started.elapsed(),
    }
}

fn examine(work_tree: &Path, current: Option<&Path>, options: WorktreeOptions) -> RepositoryWorktrees {
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
    // a hundred worktrees.
    repository.worktrees = entries
        .par_iter()
        .skip(1)
        .filter(|entry| !entry.bare)
        .map(|entry| judge(entry, &repository.repository, &default_sha, current, options))
        .collect();
    repository.worktrees.sort_by(|left, right| left.path.cmp(&right.path));
    repository
}

fn judge(entry: &Entry, main: &Path, default_sha: &str, current: Option<&Path>, options: WorktreeOptions) -> Worktree {
    let mut worktree = Worktree {
        path: entry.path.clone(),
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
    let canonical = entry.path.canonicalize().unwrap_or_else(|_| entry.path.clone());
    if current.is_some_and(|cwd| cwd.starts_with(&canonical)) {
        worktree.state = State::Current;
        return worktree;
    }
    match is_ancestor(main, &entry.head, default_sha) {
        Ok(true) => {}
        Ok(false) => return worktree,
        Err(message) => {
            worktree.state = State::Unchecked(message);
            return worktree;
        }
    }
    let Some(status) = git_bytes(
        &entry.path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
    ) else {
        worktree.state = State::Unchecked("`git status` failed".to_owned());
        return worktree;
    };
    let (discarded, changed) = classify_status(&status);
    if changed > 0 {
        worktree.state = State::LocalChanges { changed };
        return worktree;
    }
    worktree.state = State::Merged { discarded };
    worktree.outcome = Some(if options.dry_run {
        Outcome::WouldRemove
    } else {
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
        if index == b' ' && tree == b'D' && is_build_output(&path) {
            discarded += 1;
        } else {
            changed += 1;
        }
    }
    (discarded, changed)
}

fn is_build_output(path: &str) -> bool {
    path.split('/').any(|part| BUILD_OUTPUT_DIRS.contains(&part))
}

fn remove(main: &Path, path: &Path, force: bool) -> Outcome {
    let mut command = base_command(main);
    command.args(["worktree", "remove"]);
    if force {
        command.arg("--force");
    }
    command.arg(path);
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

/// A git command that cannot run repository hooks or prompt.
fn base_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args(["-c", "core.hooksPath="])
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
