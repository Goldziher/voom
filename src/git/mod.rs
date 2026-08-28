//! Git housekeeping — the maintenance git itself already considers safe.
//!
//! A repository accumulates slack voom can reclaim without deciding anything for the user:
//! administrative files for worktrees that no longer exist, and loose objects that belong in a
//! pack. Git has commands for both, they are idempotent, and they read the user's own
//! configuration. So voom **shells out to `git`** and runs them; it does not link a git library
//! and does not reimplement reachability. See `adrs/0011-git-housekeeping.md`.
//!
//! What is deliberately *not* run is the more interesting half: no `git reflog expire` beyond
//! what git's own `gc` does, no `--prune=now`, no `gc --aggressive`, and no `git remote prune`
//! unless it is asked for by name — that one contacts the network. The reflog is how somebody
//! gets back a commit they thought they had lost, and a housekeeping command that empties it has
//! turned a recovery mechanism into a liability. "Prunable, not force-remove" is the whole brief,
//! and it is not a conservatism to relax later.
//!
//! There are two surfaces. An ordinary sweep runs the two local steps as it goes, reusing the
//! repositories the artifact walker passed anyway ([`repository_at`]) so discovery costs nothing;
//! `voom git-prune` is the explicit surface, walks for itself, and can be asked for the network
//! step.
//!
//! This module holds the option types and the per-repository step sequence. Finding
//! repositories and proving what they are lives in [`discover`]; starting `git`, bounding it in
//! time and reading what it said lives in [`run`]; rendering lives in [`report`].

mod discover;
mod report;
mod run;

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::report::exit;
use crate::scan::PatternSet;

pub use discover::repository_at;
pub use report::{document, render_human, render_json, write_sweep_block};

use discover::{Located, Resolution};
use run::{ensure_git_available, run_step};

/// The JSON shape's version, versioned separately from the sweep's because it is a separate
/// document sharing only the envelope.
pub const SCHEMA_VERSION: u32 = 1;

/// The name of the entry that proves a repository.
const DOT_GIT: &str = ".git";

/// How long one repository's housekeeping may take before voom kills it.
///
/// [`Command`] has no timeout of its own, and git against a repository on a stalled network mount
/// does not return — one such repository would otherwise wedge a sweep of a home directory
/// indefinitely. Five minutes is well past anything `gc --auto` does on local storage, and short
/// enough that a hung mount costs one pause rather than the run.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Anything git housekeeping can fail at outright, as opposed to per-repository.
///
/// Its own enum rather than a variant of [`crate::error::Error`]: every one of these is about the
/// external `git` process, which nothing else in the library knows exists.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitError {
    /// `git` is not on `PATH`. Checked once, before anything is run.
    #[error("`git` was not found on PATH — voom delegates git housekeeping to git itself")]
    NotInstalled,

    /// `git` could not be started at all.
    #[error("running `git`")]
    Spawn {
        /// Why it would not start.
        #[source]
        source: io::Error,
    },

    /// A scan root could not be resolved. A root that does not exist is a usage error.
    #[error("resolving `{}`", path.display())]
    Root {
        /// The root as it was given.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: io::Error,
    },

    /// The worker pool could not be built. Fatal for the same reason the sweep's is: `-j` exists
    /// to keep voom off a spinning disk, so ignoring it would do what the user asked it not to.
    #[error("building a worker pool of {jobs} threads")]
    ThreadPool {
        /// How many threads were asked for.
        jobs: usize,
        /// The underlying failure.
        #[source]
        source: rayon::ThreadPoolBuildError,
    },
}

/// What to run, and where.
#[expect(
    clippy::struct_excessive_bools,
    reason = "these mirror command-line flags, which are bools by nature"
)]
#[derive(Debug)]
pub struct GitPruneOptions {
    /// Trees to search for repositories, and the bound containment is checked against.
    pub roots: Vec<PathBuf>,
    /// Report what git would prune, without letting it repack.
    ///
    /// `worktree prune` still *runs*, under `-n`, which is git's own dry run and reports what it
    /// would have removed. Nothing is repacked.
    pub dry_run: bool,
    /// Worker threads. `None` means available parallelism.
    pub jobs: Option<usize>,
    /// Whether the discovery walk stays on the root's filesystem.
    pub one_file_system: bool,
    /// How long one repository's housekeeping may take.
    pub timeout: Duration,
    /// Paths never searched.
    pub exclude: PatternSet,
    /// Whether to search machine-global tool caches and installed toolchains too.
    pub caches: bool,
    /// Whether to prune remote-tracking branches whose upstream is gone.
    ///
    /// **Off by default, and not available to a sweep at all.** `git remote prune` contacts the
    /// remote: over a home directory that is one network round trip per remote per repository, it
    /// hangs without connectivity, and it can block on credentials for a private remote.
    pub remotes: bool,
    /// How old a worktree's administration must be before git may prune it.
    ///
    /// Defaults to [`WORKTREE_PRUNE_EXPIRE`]. Any value `git worktree prune --expire` accepts.
    pub worktree_expire: String,
    /// Whether a dry run reports `git count-objects -v` for each repository.
    ///
    /// The explicit subcommand does; a sweep does not, because a per-repository object census
    /// across a home directory would bury the artifact report it is attached to.
    pub count_objects: bool,
}

impl Default for GitPruneOptions {
    /// The sweep's policy: both local steps, no network, no census.
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            dry_run: false,
            jobs: None,
            one_file_system: true,
            timeout: DEFAULT_TIMEOUT,
            exclude: PatternSet::default(),
            caches: false,
            remotes: false,
            worktree_expire: WORKTREE_PRUNE_EXPIRE.to_owned(),
            count_objects: false,
        }
    }
}

/// Why a repository was passed over without running anything.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Skipped {
    /// A git operation is half-finished here.
    #[error("a git operation is in progress ({marker})")]
    InProgress {
        /// The marker that proves it, relative to the repository.
        marker: String,
    },
    /// The path is on the protected denylist (ADR 0006).
    #[error("a protected path")]
    Protected,
    /// The repository does not resolve below any scan root.
    #[error("outside every scan root")]
    EscapesRoot,
    /// A `.git` file naming a repository shape voom does not recognise.
    #[error("voom could not resolve its `.git` file: {detail}")]
    Unresolved {
        /// What went wrong reading or interpreting it.
        detail: String,
    },
    /// The repository disappeared between the walk and the check.
    #[error("it vanished during the run")]
    Vanished,
}

impl Skipped {
    /// The enum-like code a machine consumer branches on.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::InProgress { .. } => "operation_in_progress",
            Self::Protected => "protected",
            Self::EscapesRoot => "escapes_root",
            Self::Unresolved { .. } => "unresolved",
            Self::Vanished => "vanished",
        }
    }
}

/// Which piece of housekeeping a step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// `git worktree prune` — administrative files for worktrees that are gone.
    WorktreePrune,
    /// `git remote` — reading the remotes, which is not housekeeping but can fail.
    RemoteList,
    /// `git remote prune <remote>` — remote-tracking branches whose upstream is gone.
    RemotePrune,
    /// `git gc --auto` — repack, on git's own thresholds.
    Gc,
    /// `git count-objects -v` — what a repack would have to work with, under `--dry-run`.
    CountObjects,
}

impl StepKind {
    /// The enum-like code a machine consumer branches on.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::WorktreePrune => "worktree_prune",
            Self::RemoteList => "remote_list",
            Self::RemotePrune => "remote_prune",
            Self::Gc => "gc",
            Self::CountObjects => "count_objects",
        }
    }

    /// The step's name in the report, without the remote a `remote prune` carries.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::WorktreePrune => "worktree prune",
            Self::RemoteList => "remote",
            Self::RemotePrune => "remote prune",
            Self::Gc => "gc",
            Self::CountObjects => "count-objects",
        }
    }
}

/// What became of one git command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// git exited zero. The summary is a one-line reading of its output, empty when it had
    /// nothing to say — which for a `gc --auto` below its thresholds is the normal case.
    Succeeded {
        /// What it reported, in words.
        summary: String,
    },
    /// git exited non-zero.
    Failed {
        /// The exit status, when there was one rather than a signal.
        code: Option<i32>,
        /// git's own complaint, trimmed to its first meaningful lines.
        message: String,
    },
    /// The repository's time budget ran out and the child was killed.
    TimedOut,
}

/// One git command run against one repository.
#[derive(Debug, Clone)]
pub struct Step {
    /// Which piece of housekeeping this is.
    pub kind: StepKind,
    /// The remote, for a `remote prune`.
    pub remote: Option<String>,
    /// The argv exactly as it was run — the report's record of what voom asked git to do.
    pub command: Vec<String>,
    /// What happened.
    pub outcome: StepOutcome,
}

impl Step {
    /// The step's name in the report: `worktree prune`, `remote prune origin`, `gc`.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.remote {
            Some(remote) => format!("{} {remote}", self.kind.label()),
            None => self.kind.label().to_owned(),
        }
    }

    /// Whether this step did what it was asked.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(self.outcome, StepOutcome::Succeeded { .. })
    }

    /// What it had to say, when it had anything.
    #[must_use]
    pub fn summary(&self) -> Option<&str> {
        match &self.outcome {
            StepOutcome::Succeeded { summary } if !summary.is_empty() => Some(summary),
            _ => None,
        }
    }
}

/// What became of one repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Every step voom asked for succeeded.
    Pruned,
    /// Nothing was run: a rail refused it, or an operation is in progress.
    Skipped,
    /// At least one step failed or timed out.
    Failed,
}

/// One repository and what became of it.
#[derive(Debug, Clone)]
pub struct Repository {
    /// The working tree git was run in, canonicalized — the form containment resolved and
    /// deduplication compared, so the form the report states.
    pub path: PathBuf,
    /// Why nothing was run, when nothing was.
    pub skipped: Option<Skipped>,
    /// The git commands run, in order.
    pub steps: Vec<Step>,
}

impl Repository {
    /// What became of it.
    #[must_use]
    pub fn status(&self) -> Status {
        if self.skipped.is_some() {
            return Status::Skipped;
        }
        if self.steps.iter().all(Step::succeeded) {
            Status::Pruned
        } else {
            Status::Failed
        }
    }

    /// Whether this repository is worth a line in a *sweep's* report.
    ///
    /// A default sweep walks past every repository in a home directory and `gc --auto` is a
    /// no-op in nearly all of them. Silence is the correct output for a no-op: a git section that
    /// always appeared would be noise on the one screen that has to stay readable, so only
    /// repositories where something happened, or where a rail fired, get a line. The explicit
    /// subcommand reports all of them — there the user asked.
    #[must_use]
    pub fn is_noteworthy(&self) -> bool {
        match self.status() {
            Status::Failed => true,
            // A repository that disappeared mid-run is not something the reader can act on.
            Status::Skipped => self.skipped != Some(Skipped::Vanished),
            Status::Pruned => self.steps.iter().any(|step| step.summary().is_some()),
        }
    }
}

/// The footer's numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Repositories found.
    pub repositories: usize,
    /// Repositories whose housekeeping ran and succeeded.
    pub pruned: usize,
    /// Repositories passed over.
    pub skipped: usize,
    /// Repositories where a step failed or timed out.
    pub failed: usize,
}

/// Everything one round of housekeeping produced.
#[derive(Debug)]
pub struct GitPruneResult {
    /// The roots that were searched, canonicalized.
    pub roots: Vec<PathBuf>,
    /// Repositories, sorted by path.
    pub repositories: Vec<Repository>,
    /// Whether the repack was withheld.
    pub dry_run: bool,
    /// Wall-clock time.
    pub elapsed: Duration,
}

impl GitPruneResult {
    /// The footer's numbers.
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals {
            repositories: self.repositories.len(),
            ..Totals::default()
        };
        for repository in &self.repositories {
            match repository.status() {
                Status::Pruned => totals.pruned += 1,
                Status::Skipped => totals.skipped += 1,
                Status::Failed => totals.failed += 1,
            }
        }
        totals
    }

    /// The repositories a sweep's report should print, in order. Empty means print nothing.
    #[must_use]
    pub fn noteworthy(&self) -> Vec<&Repository> {
        self.repositories
            .iter()
            .filter(|repository| repository.is_noteworthy())
            .collect()
    }

    /// Folds another round's repositories into this one.
    ///
    /// A sweep runs the pipeline once per root, so a two-root run produces one of these per root
    /// and the report wants one. Deduplication has already happened within each round; a
    /// repository reachable from two roots is dropped here, on the same canonical path.
    pub fn merge(&mut self, other: Self) {
        for root in other.roots {
            if !self.roots.contains(&root) {
                self.roots.push(root);
            }
        }
        for repository in other.repositories {
            if !self.repositories.iter().any(|seen| seen.path == repository.path) {
                self.repositories.push(repository);
            }
        }
        self.repositories.sort_by(|left, right| left.path.cmp(&right.path));
        self.elapsed += other.elapsed;
    }

    /// The process exit code: zero unless a repository failed.
    ///
    /// A skip is a rail doing its job, not a failure, and exits zero — the same reading
    /// [`crate::report::RunResult::exit_code`] gives a refusal.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if self.totals().failed > 0 {
            exit::REMOVAL_FAILED
        } else {
            exit::SUCCESS
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

/// Finds every repository under the roots and runs git's housekeeping in each.
///
/// This is `voom git-prune`, which walks for itself because it can be pointed at a tree no sweep
/// is running over.
///
/// # Errors
///
/// Whatever [`prune_repositories`] returns.
pub fn prune(options: &GitPruneOptions) -> Result<GitPruneResult, GitError> {
    let started = Instant::now();
    let work_trees = discover::repositories(options);
    let mut result = prune_repositories(&work_trees, options)?;
    result.elapsed = started.elapsed();
    Ok(result)
}

/// Runs git's housekeeping in each of the given working trees.
///
/// The paths are whatever a walk produced — a main repository, a linked worktree, a submodule.
/// Each is resolved to the repository git would actually act on and deduplicated, so a repository
/// with four checked-out worktrees is pruned once and not four times.
///
/// One repository's failure is recorded and never aborts the run.
///
/// # Errors
///
/// [`GitError::NotInstalled`] when `git` is not on `PATH`, [`GitError::Spawn`] when it cannot be
/// started, [`GitError::Root`] when a scan root does not resolve, and [`GitError::ThreadPool`]
/// when `-j` cannot be honoured.
pub fn prune_repositories(work_trees: &[PathBuf], options: &GitPruneOptions) -> Result<GitPruneResult, GitError> {
    prune_with(work_trees, options, true)
}

/// The sweep's entry point, which never fails the run.
///
/// Returns `None` when there is nothing to say: no repositories were walked past, or this machine
/// has no git. Neither is a reason to fail a sweep that reclaimed disk, and neither is worth a
/// line of output.
///
/// The fan-out runs on the **caller's** rayon pool, which `run.rs` has already sized from `-j`.
/// Building a second pool nested inside it would double the thread count the flag asked for, so
/// `jobs` is deliberately not honoured here — it already was, one level up.
#[must_use]
pub fn prune_during_sweep(work_trees: &[PathBuf], options: &GitPruneOptions) -> Option<GitPruneResult> {
    if work_trees.is_empty() {
        return None;
    }
    prune_with(work_trees, options, false).ok()
}

fn prune_with(work_trees: &[PathBuf], options: &GitPruneOptions, own_pool: bool) -> Result<GitPruneResult, GitError> {
    let started = Instant::now();
    let roots = canonical_roots(&options.roots)?;
    let mut result = GitPruneResult {
        roots,
        repositories: Vec::new(),
        dry_run: options.dry_run,
        elapsed: Duration::default(),
    };
    if work_trees.is_empty() {
        return Ok(result);
    }
    // Once, before anything is spawned: a missing git is a usage error, not a per-repository one,
    // and reporting it a thousand times over would bury it.
    ensure_git_available(options.timeout)?;

    let resolved = discover::resolve_all(work_trees, &result.roots);
    result.repositories = match options.jobs.filter(|_| own_pool) {
        Some(jobs) => rayon::ThreadPoolBuilder::new()
            .num_threads(jobs.max(1))
            .build()
            .map_err(|source| GitError::ThreadPool { jobs, source })?
            .install(|| visit_all(&resolved, options)),
        None => visit_all(&resolved, options),
    };
    // Parallel work finishes in nondeterministic order, and a report that cannot be diffed
    // between two runs is not a report (ADR 0007).
    result.repositories.sort_by(|left, right| left.path.cmp(&right.path));
    result.elapsed = started.elapsed();
    Ok(result)
}

/// The standalone discovery walk, for the subcommand.
#[must_use]
pub fn discover_repositories(options: &GitPruneOptions) -> Vec<PathBuf> {
    discover::repositories(options)
}

fn visit_all(resolved: &[Resolution], options: &GitPruneOptions) -> Vec<Repository> {
    // Fanned out per repository, the way sizing and removal are: each one is an independent
    // subprocess, and the timeout bounds each independently.
    resolved
        .par_iter()
        .map(|resolution| match resolution {
            Resolution::Repository(located) => visit(located, options),
            Resolution::Refused(path, skipped) => Repository {
                path: path.clone(),
                skipped: Some(skipped.clone()),
                steps: Vec::new(),
            },
        })
        .collect()
}

fn canonical_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>, GitError> {
    roots
        .iter()
        .map(|root| {
            root.canonicalize().map_err(|source| GitError::Root {
                path: root.clone(),
                source,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The step sequence
// ---------------------------------------------------------------------------------------------

fn visit(located: &Located, options: &GitPruneOptions) -> Repository {
    let path = located.work_tree().to_path_buf();
    let common_dir = located.common_dir().to_path_buf();
    if let Some(marker) = discover::in_progress(located.common_dir()) {
        let marker = marker
            .strip_prefix(&path)
            .unwrap_or(&marker)
            .display()
            .to_string()
            .replace(std::path::MAIN_SEPARATOR, "/");
        return Repository {
            path,
            skipped: Some(Skipped::InProgress { marker }),
            steps: Vec::new(),
        };
    }

    // One budget for the repository, shared by its steps. A per-command timeout would let a
    // repository with a dozen remotes take a dozen times as long as the number the flag names.
    let deadline = Instant::now() + options.timeout;
    let worktree_prune = worktree_prune_args(options.dry_run, &options.worktree_expire);
    let borrowed: Vec<&str> = worktree_prune.iter().map(String::as_str).collect();
    let mut steps = vec![run_step(
        &path,
        &common_dir,
        StepKind::WorktreePrune,
        None,
        &borrowed,
        deadline,
    )];

    if options.remotes && steps.iter().all(Step::succeeded) {
        prune_remotes(&path, &common_dir, options.dry_run, deadline, &mut steps);
    }
    if steps.iter().all(Step::succeeded)
        && let Some((kind, args)) = repack_step(options)
    {
        steps.push(run_step(&path, &common_dir, kind, None, &args, deadline));
    }

    Repository {
        path,
        skipped: None,
        steps,
    }
}

/// How old a worktree's administrative files must be before voom will let git prune them.
///
/// This is git's own `gc.worktreePruneExpire` default, and passing it is the difference between
/// deferring to git and being more destructive than it. **`git worktree prune` on its own
/// expires everything**: it removes the administration for every worktree whose directory is
/// absent right now, at any age. `git gc` — the command voom is otherwise delegating to — never
/// does that; it prunes with this expiry.
///
/// A worktree directory is absent for entirely ordinary reasons: an unmounted external disk, a
/// network share that is not up yet, an encrypted volume not yet opened. And
/// `.git/worktrees/<name>` holds that worktree's `HEAD` and **its reflog**, which is the
/// recovery path for anything committed there and not merged — so removing it makes that work
/// unreachable, and the `gc --auto` in the same visit starts the clock on collecting it.
///
/// A repository that configures its own `gc.worktreePruneExpire` is not consulted; reading it
/// would cost a subprocess per repository. Three months is the conservative direction.
pub const WORKTREE_PRUNE_EXPIRE: &str = "3.months.ago";

/// `worktree prune`, verbose in both modes so the report can say what was removed.
fn worktree_prune_args(dry_run: bool, expire: &str) -> Vec<String> {
    let mut args = vec!["worktree", "prune", "--expire", expire, "-v"];
    if dry_run {
        // `-n` is git's own dry run: it really runs, and reports what it would have removed.
        args.insert(2, "-n");
    }
    args.into_iter().map(ToOwned::to_owned).collect()
}

/// The repack step, or `None` when a dry run is not asking for the object census.
///
/// **`gc --auto`, never a bare `gc`.** `--auto` is what git itself runs after fetch, commit,
/// merge and rebase: it checks its own thresholds and is a cheap no-op on a repository that does
/// not need repacking. A bare `gc` repacks unconditionally, which over a home directory would
/// cost minutes and break the claim this tool is built on.
fn repack_step(options: &GitPruneOptions) -> Option<(StepKind, Vec<&'static str>)> {
    match (options.dry_run, options.count_objects) {
        (false, _) => Some((StepKind::Gc, vec!["gc", "--auto"])),
        (true, true) => Some((StepKind::CountObjects, vec!["count-objects", "-v"])),
        (true, false) => None,
    }
}

fn prune_remotes(path: &Path, git_dir: &Path, dry_run: bool, deadline: Instant, steps: &mut Vec<Step>) {
    let listing = run_step(path, git_dir, StepKind::RemoteList, None, &["remote"], deadline);
    let Some(names) = listing.summary().map(remote_names) else {
        // A listing that succeeded with no output means no remotes, and leaves no trace in the
        // report; one that failed is the repository's failure and is kept.
        if !listing.succeeded() {
            steps.push(listing);
        }
        return;
    };
    for remote in names {
        let args = remote_prune_args(dry_run, &remote);
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let step = run_step(path, git_dir, StepKind::RemotePrune, Some(remote), &borrowed, deadline);
        let succeeded = step.succeeded();
        steps.push(step);
        if !succeeded {
            return;
        }
    }
}

fn remote_names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn remote_prune_args(dry_run: bool, remote: &str) -> Vec<String> {
    let mut args = vec!["remote".to_owned(), "prune".to_owned()];
    if dry_run {
        args.push("-n".to_owned());
    }
    args.push(remote.to_owned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default repack step is git's own, which is a no-op below git's thresholds. A bare `gc`
    /// would repack every repository in a home directory unconditionally.
    #[test]
    fn should_repack_only_on_gits_own_thresholds() {
        let (kind, args) = repack_step(&GitPruneOptions::default()).expect("a repack step");
        assert_eq!(kind, StepKind::Gc);
        assert_eq!(args, vec!["gc", "--auto"], "never a bare `gc`");
    }

    /// The default step list is strictly local. `git remote prune` contacts the remote, so a
    /// sweep over a home directory must never build one.
    #[test]
    fn should_build_no_networked_command_by_default() {
        let options = GitPruneOptions::default();
        assert!(!options.remotes, "the network step is opt-in");
        let mut argv = worktree_prune_args(options.dry_run, &options.worktree_expire);
        argv.extend(
            repack_step(&options)
                .into_iter()
                .flat_map(|(_, args)| args.into_iter().map(ToOwned::to_owned)),
        );
        assert_eq!(
            argv,
            vec!["worktree", "prune", "--expire", "3.months.ago", "-v", "gc", "--auto"]
        );
    }

    /// The expiry is git's own `gc.worktreePruneExpire` default and never `worktree prune`'s.
    /// Left off, the step removes the administration — and the reflog — of every worktree whose
    /// checkout happens to be absent, at any age, which an unmounted disk is enough to cause.
    #[test]
    fn should_prune_worktrees_on_gits_own_expiry_and_never_on_none() {
        assert_eq!(GitPruneOptions::default().worktree_expire, "3.months.ago");
        let argv = worktree_prune_args(false, WORKTREE_PRUNE_EXPIRE);
        let expiry = argv.iter().position(|arg| arg.as_str() == "--expire");
        assert!(expiry.is_some(), "the step always carries an expiry: {argv:?}");
    }

    /// A sweep's dry run reports nothing per repository: an object census across a home directory
    /// would bury the artifact report it is attached to.
    #[test]
    fn should_withhold_the_object_census_from_a_sweeps_dry_run() {
        let sweep = GitPruneOptions {
            dry_run: true,
            ..GitPruneOptions::default()
        };
        assert!(repack_step(&sweep).is_none());

        let subcommand = GitPruneOptions {
            dry_run: true,
            count_objects: true,
            ..GitPruneOptions::default()
        };
        assert_eq!(
            repack_step(&subcommand).map(|(kind, _)| kind),
            Some(StepKind::CountObjects)
        );
    }
}
