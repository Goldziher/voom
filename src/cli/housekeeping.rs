//! Arguments and rendering for the maintenance subcommands: `git-prune`,
//! `claude-prune`, `docker-prune`. Split out of `cli/mod.rs` purely to stay under the module size cap — the
//! share a shape (a subcommand takes its own paths, its own `--dry-run`, its own
//! `--format`) but nothing that would justify one being written in terms of another.

use std::io;
use std::path::PathBuf;

use clap::Args;

use super::{Format, PruneArgs};
use crate::docker::VolumeScope;
use crate::error::Result;
use crate::policy::parse_duration;

/// `voom git-prune` arguments.
#[expect(
    clippy::struct_excessive_bools,
    reason = "these mirror command-line flags, which are bools by nature"
)]
#[derive(Debug, Args)]
pub struct GitPruneArgs {
    /// Trees to search for repositories.
    #[arg(value_name = "PATH", default_value = ".")]
    pub paths: Vec<PathBuf>,

    /// Report what git would prune, without letting it repack.
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Output format.
    ///
    /// Duplicated from the top-level flag rather than inherited: `args_conflicts_with_subcommands`
    /// makes `voom --format json git-prune .` a usage error, so without this the subcommand has
    /// no route to JSON at all.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    pub format: Format,

    /// Also prune remote-tracking branches whose upstream is gone.
    ///
    /// Off by default, and deliberately not part of a sweep, because it contacts the remote: one
    /// network round trip per remote per repository, which hangs without connectivity and can
    /// block on a credential prompt.
    #[arg(long)]
    pub remotes: bool,

    /// How long one repository's housekeeping may take, e.g. `30s`.
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,

    /// Also remove linked worktrees whose work is already merged into the default branch.
    ///
    /// The same rules as the top-level `--remove-merged-worktrees`: merged, not locked or the
    /// current directory, below a searched path, and clean apart from deleted tracked build
    /// output or ignored build-output trees. Reports only under `-n`.
    #[arg(long)]
    pub remove_merged_worktrees: bool,

    /// With `--remove-merged-worktrees`, also recognize squash and rebase merges on GitHub.
    ///
    /// Contacts github.com through `gh`. Only an exact HEAD match to a merged pull request
    /// in the origin repository counts; lookup failures keep the checkout. Working-tree and
    /// containment checks still apply. Commits proved only by GitHub are retained under
    /// `refs/voom/merged-worktrees/` before removal.
    #[arg(long, requires = "remove_merged_worktrees")]
    pub github_merged_worktrees: bool,

    /// How old a worktree's administration must be before git may prune it.
    ///
    /// Defaults to git's own `gc.worktreePruneExpire` policy, three months, rather than
    /// `git worktree prune`'s much more aggressive one — which removes the administration for
    /// every absent worktree at any age, an unmounted disk included, and with it the reflog that
    /// is the recovery path for anything committed there.
    #[arg(long, value_name = "TIME")]
    pub expire: Option<String>,
}

impl GitPruneArgs {
    /// The options for an explicit `voom git-prune`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidDuration`] for an unparseable timeout, or [`Error::CatalogPattern`] for
    /// an `--exclude` glob that will not compile.
    pub fn to_git_options(&self, prune: &PruneArgs) -> Result<crate::git::GitPruneOptions> {
        Ok(crate::git::GitPruneOptions {
            roots: self.paths.clone(),
            dry_run: self.dry_run,
            jobs: prune.jobs,
            one_file_system: prune.one_file_system,
            timeout: self
                .timeout
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(crate::git::DEFAULT_TIMEOUT),
            exclude: crate::scan::PatternSet::new(prune.exclude.clone())?,
            remotes: self.remotes,
            worktree_expire: self
                .expire
                .clone()
                .unwrap_or_else(|| crate::git::WORKTREE_PRUNE_EXPIRE.to_owned()),
            // The explicit surface says what a withheld repack would have had to work with; a
            // sweep does not, because the census is a question nobody asked it.
            count_objects: true,
        })
    }
}

/// Renders a finished `git-prune` in the requested format.
///
/// # Errors
///
/// Propagates write failures from `out`.
pub fn render_git(
    result: &crate::git::GitPruneResult,
    args: &GitPruneArgs,
    out: &mut impl io::Write,
) -> io::Result<()> {
    match args.format {
        Format::Human => crate::git::render_human(result, out),
        Format::Json => crate::git::render_json(result, out),
    }
}

/// `voom claude-prune` arguments.
#[expect(
    clippy::struct_excessive_bools,
    reason = "these mirror command-line flags, which are bools by nature"
)]
#[derive(Debug, Args)]
pub struct ClaudePruneArgs {
    /// `~/.claude/jobs`-shaped directories to search. Defaults to `~/.claude/jobs`.
    #[arg(value_name = "PATH")]
    pub roots: Vec<PathBuf>,

    /// Actually remove an eligible job's scratch, on top of the age gate and the liveness check
    /// both already having to hold. Without this, the command only ever reports.
    #[arg(long)]
    pub remove: bool,

    /// Report what would be removed, without removing it. Only meaningful with `--remove`.
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// How long a job must have been quiet before it is even a candidate.
    #[arg(long, value_name = "DURATION")]
    pub min_age: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    pub format: Format,

    /// Do not cross filesystem boundaries.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub one_file_system: bool,

    /// Retry a failed removal, repairing permissions inside the scratch directory first.
    #[arg(long)]
    pub force: bool,
}

impl ClaudePruneArgs {
    /// The options for `voom claude-prune`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidDuration`] for an unparseable `--min-age`.
    pub fn to_claude_options(&self) -> Result<crate::claude::ClaudePruneOptions> {
        Ok(crate::claude::ClaudePruneOptions {
            roots: if self.roots.is_empty() {
                crate::claude::default_root()
            } else {
                self.roots.clone()
            },
            min_age: self
                .min_age
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(crate::claude::DEFAULT_MIN_AGE),
            remove: self.remove,
            dry_run: self.dry_run,
            force: self.force,
            one_file_system: self.one_file_system,
        })
    }
}

/// Renders a finished `claude-prune` in the requested format.
///
/// # Errors
///
/// Propagates write failures from `out`.
pub fn render_claude(
    result: &crate::claude::ClaudePruneResult,
    args: &ClaudePruneArgs,
    out: &mut impl io::Write,
) -> io::Result<()> {
    match args.format {
        Format::Human => crate::claude::render_human(result, out),
        Format::Json => crate::claude::render_json(result, out),
    }
}

/// `voom docker-prune` arguments.
#[expect(
    clippy::struct_excessive_bools,
    reason = "these mirror command-line flags, which are bools by nature"
)]
#[derive(Debug, Args)]
pub struct DockerPruneArgs {
    /// Report what would be reclaimed, without pruning or trimming anything.
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    pub format: Format,

    /// How old an unused image or idle build-cache record must be before it is pruned, e.g. `7d`.
    ///
    /// Dangling images go regardless of age. Mapped to docker's `until=` filter in whole hours,
    /// rounded up.
    #[arg(long, value_name = "DURATION")]
    pub max_age: Option<String>,

    /// Also remove anonymous volumes no container mounts. Volumes are data: off unless given.
    ///
    /// Only volumes docker created without a name, which nothing can find again. Named volumes
    /// need `--volumes-all`.
    #[arg(long)]
    pub volumes: bool,

    /// Also remove every volume no container mounts, named ones included. Implies `--volumes`.
    #[arg(long)]
    pub volumes_all: bool,

    /// Do not trim the VM's disk afterwards.
    ///
    /// On Colima, Lima, Rancher Desktop and podman machine, freed space stays allocated on the
    /// host until `fstrim` runs in the guest.
    #[arg(long)]
    pub no_trim: bool,

    /// How long each prune command and the trim may run, e.g. `15m`.
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,
}

impl DockerPruneArgs {
    /// The options for an explicit `voom docker-prune`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidDuration`] for an unparseable `--max-age` or `--timeout`.
    pub fn to_docker_options(&self) -> Result<crate::docker::DockerPruneOptions> {
        Ok(crate::docker::DockerPruneOptions {
            dry_run: self.dry_run,
            max_age: self
                .max_age
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(crate::docker::DEFAULT_MAX_AGE),
            volumes: volume_scope(self.volumes, self.volumes_all),
            trim: !self.no_trim,
            timeout: self
                .timeout
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(crate::docker::DEFAULT_TIMEOUT),
        })
    }
}

/// The volume scope two flags ask for: `--volumes-all` wins and implies `--volumes`, and neither
/// leaves volumes alone entirely.
#[must_use]
pub fn volume_scope(volumes: bool, all: bool) -> Option<VolumeScope> {
    match (volumes, all) {
        (_, true) => Some(VolumeScope::All),
        (true, false) => Some(VolumeScope::Anonymous),
        (false, false) => None,
    }
}

/// Renders a finished `docker-prune` in the requested format.
///
/// # Errors
///
/// Propagates write failures from `out`.
pub fn render_docker(
    result: &crate::docker::DockerPruneResult,
    args: &DockerPruneArgs,
    out: &mut impl io::Write,
) -> io::Result<()> {
    match args.format {
        Format::Human => crate::docker::render_human(result, out),
        Format::Json => crate::docker::render_json(result, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_leave_volumes_alone_unless_a_flag_asks() {
        assert_eq!(volume_scope(false, false), None);
    }

    #[test]
    fn should_remove_only_anonymous_volumes_for_the_plain_flag() {
        assert_eq!(volume_scope(true, false), Some(VolumeScope::Anonymous));
    }

    #[test]
    fn should_let_the_all_flag_imply_volumes_and_win() {
        assert_eq!(volume_scope(false, true), Some(VolumeScope::All));
        assert_eq!(volume_scope(true, true), Some(VolumeScope::All));
    }
}
