//! Rendering for [`super::WorktreePruneResult`]: one row per worktree worth naming, then a footer.

use std::io;

use owo_colors::OwoColorize;
use serde_json::{Value, json};

use super::{Outcome, SCHEMA_VERSION, State, Worktree, WorktreePruneResult};

/// Renders the human report. Without `verbose`, worktrees that are simply kept (not merged,
/// locked, missing, outside the swept paths) are counted, not listed; with it, each is named with
/// the reason it was kept.
///
/// # Errors
///
/// Propagates write failures from `out`.
pub fn render_human(result: &WorktreePruneResult, verbose: bool, out: &mut impl io::Write) -> io::Result<()> {
    for repository in &result.repositories {
        if let Some(error) = &repository.error {
            writeln!(
                out,
                "  {}  {} — {error}",
                "skipped".yellow(),
                repository.repository.display()
            )?;
            continue;
        }
        for worktree in &repository.worktrees {
            if let Some((label, detail)) = row(worktree, result.dry_run, verbose, repository.default_branch.as_deref())
            {
                writeln!(out, "  {label}  {} — {detail}", worktree.path.display())?;
            }
        }
    }
    let totals = result.totals();
    let verb = if result.dry_run { "would be removed" } else { "removed" };
    writeln!(
        out,
        "{} merged or stale worktrees {verb}, {} with local changes, {} others kept in {:.2?}",
        totals.removed.bold(),
        totals.local_changes,
        totals.kept,
        result.elapsed
    )?;
    if totals.dangling > 0 {
        let text = format!(
            "{} dangling checkout(s) not removed: their git administration is gone",
            totals.dangling
        );
        writeln!(out, "  {}", text.yellow())?;
    }
    if totals.failed > 0 {
        writeln!(out, "  {}", format!("{} failed", totals.failed).red())?;
    }
    Ok(())
}

fn row(worktree: &Worktree, dry_run: bool, verbose: bool, default_branch: Option<&str>) -> Option<(String, String)> {
    match (&worktree.state, &worktree.outcome) {
        (State::Merged { discarded } | State::Stale { discarded }, Some(Outcome::Removed | Outcome::WouldRemove)) => {
            Some((
                if dry_run {
                    "would remove".yellow().to_string()
                } else {
                    "removed".green().to_string()
                },
                merged_detail(worktree, *discarded, matches!(worktree.state, State::Stale { .. })),
            ))
        }
        (State::Merged { .. } | State::Stale { .. }, Some(Outcome::Failed(message))) => {
            Some(("failed".red().to_string(), message.clone()))
        }
        (State::Dangling { .. }, _) => Some(("dangling".yellow().to_string(), reason(&worktree.state, default_branch))),
        // A kept worktree is named when the reader can act on it, or asked to see everything.
        (State::LocalChanges { .. } | State::Unchecked(_) | State::HeadDiffers { .. }, _) => {
            Some(("kept".cyan().to_string(), reason(&worktree.state, default_branch)))
        }
        _ if verbose => Some(("kept".cyan().to_string(), reason(&worktree.state, default_branch))),
        _ => None,
    }
}

/// Why a worktree was kept, in words. Also the JSON `reason`.
fn reason(state: &State, default_branch: Option<&str>) -> String {
    match state {
        State::Main => "the repository's own checkout".to_owned(),
        State::Locked => "locked (`git worktree unlock` allows removal)".to_owned(),
        State::Current => "voom was started inside it".to_owned(),
        State::Missing => "its directory is gone; `git worktree prune` owns that".to_owned(),
        State::NotMerged => default_branch.map_or_else(
            || "HEAD is not merged (no default branch was found)".to_owned(),
            |branch| format!("HEAD is not in {branch}"),
        ),
        State::OutsideRoot => "stored outside every path being swept".to_owned(),
        State::LocalChanges { changed } => {
            format!("merged or stale, but {changed} changed path(s) are not build output")
        }
        State::Unchecked(message) => message.clone(),
        State::HeadDiffers { pull } => format!(
            "pull request #{pull} merged, but this HEAD is not its head (amended or extended afterwards), \
             so the merge proof does not apply"
        ),
        State::Dangling { gitdir, bytes } => format!(
            "not removed: {} on disk, and its git administration `{}` is gone, so git cannot show what it \
             held or whether it was merged; review it and delete it by hand",
            humansize::format_size(*bytes, humansize::DECIMAL),
            gitdir.display()
        ),
        State::Merged { .. } | State::Stale { .. } => "merged".to_owned(),
    }
}

fn merged_detail(worktree: &Worktree, discarded: usize, stale: bool) -> String {
    let why = if stale { "stale" } else { "merged" };
    let branch = worktree
        .branch
        .as_deref()
        .map_or("detached".to_owned(), |branch| format!("branch {branch}"));
    if discarded > 0 {
        format!("{branch}, {why} ({discarded} deleted build-output path(s) discarded)")
    } else {
        format!("{branch}, {why} and clean")
    }
}

fn state_code(state: &State) -> &'static str {
    match state {
        State::Main => "main",
        State::Locked => "locked",
        State::Current => "current",
        State::Missing => "missing",
        State::NotMerged => "not_merged",
        State::OutsideRoot => "outside_root",
        State::LocalChanges { .. } => "local_changes",
        State::Merged { .. } => "merged",
        State::Stale { .. } => "stale",
        State::Unchecked(_) => "unchecked",
        State::HeadDiffers { .. } => "head_differs",
        State::Dangling { .. } => "dangling",
    }
}

/// The reason a worktree was not removed, or `None` when it was (or would be).
fn kept_reason(worktree: &Worktree, default_branch: Option<&str>) -> Option<String> {
    match (&worktree.state, &worktree.outcome) {
        (State::Merged { .. } | State::Stale { .. }, Some(Outcome::Failed(message))) => Some(message.clone()),
        (State::Merged { .. } | State::Stale { .. }, _) => None,
        (state, _) => Some(reason(state, default_branch)),
    }
}

/// Builds the JSON document, for a sweep to embed whole under its own `worktrees` key the way it
/// embeds [`crate::git::document`].
#[must_use]
pub fn document(result: &WorktreePruneResult) -> Value {
    let totals = result.totals();
    json!({
        "schema_version": SCHEMA_VERSION,
        "dry_run": result.dry_run,
        "repositories": result.repositories.iter().map(|repository| json!({
            "repository": repository.repository.display().to_string(),
            "default_branch": repository.default_branch,
            "error": repository.error,
            "worktrees": repository.worktrees.iter().map(|worktree| json!({
                "path": worktree.path.display().to_string(),
                "branch": worktree.branch,
                "state": state_code(&worktree.state),
                "reason": kept_reason(worktree, repository.default_branch.as_deref()),
                "bytes": match &worktree.state {
                    State::Dangling { bytes, .. } => Some(*bytes),
                    _ => None,
                },
                "outcome": worktree.outcome.as_ref().map(|outcome| match outcome {
                    Outcome::Removed => "removed",
                    Outcome::WouldRemove => "would_remove",
                    Outcome::Failed(_) => "failed",
                }),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "totals": {
            "removed": totals.removed,
            "local_changes": totals.local_changes,
            "kept": totals.kept,
            "failed": totals.failed,
            "dangling": totals.dangling,
        },
        "elapsed_ms": u64::try_from(result.elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

/// Renders the document as pretty-printed JSON.
///
/// # Errors
///
/// Propagates write and serialization failures.
pub fn render_json(result: &WorktreePruneResult, out: &mut impl io::Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, &document(result))?;
    writeln!(out)
}
