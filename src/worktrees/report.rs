//! Rendering for [`super::WorktreePruneResult`]: one row per worktree worth naming, then a footer.

use std::io;

use owo_colors::OwoColorize;
use serde_json::{Value, json};

use super::{Outcome, SCHEMA_VERSION, State, Worktree, WorktreePruneResult};

/// Renders the human report. Worktrees that are simply not merged are counted, not listed.
///
/// # Errors
///
/// Propagates write failures from `out`.
pub fn render_human(result: &WorktreePruneResult, out: &mut impl io::Write) -> io::Result<()> {
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
            if let Some((label, detail)) = row(worktree, result.dry_run) {
                writeln!(out, "  {label}  {} — {detail}", worktree.path.display())?;
            }
        }
    }
    let totals = result.totals();
    let verb = if result.dry_run { "would be removed" } else { "removed" };
    writeln!(
        out,
        "{} merged worktrees {verb}, {} merged but with local changes, {} others kept in {:.2?}",
        totals.removed.bold(),
        totals.local_changes,
        totals.kept,
        result.elapsed
    )?;
    if totals.failed > 0 {
        writeln!(out, "  {}", format!("{} failed", totals.failed).red())?;
    }
    Ok(())
}

fn row(worktree: &Worktree, dry_run: bool) -> Option<(String, String)> {
    match (&worktree.state, &worktree.outcome) {
        (State::Merged { discarded }, Some(Outcome::Removed | Outcome::WouldRemove)) => Some((
            if dry_run {
                "would remove".yellow().to_string()
            } else {
                "removed".green().to_string()
            },
            merged_detail(worktree, *discarded),
        )),
        (State::Merged { .. }, Some(Outcome::Failed(message))) => Some(("failed".red().to_string(), message.clone())),
        (State::LocalChanges { changed }, _) => Some((
            "kept".cyan().to_string(),
            format!("merged, but {changed} changed path(s) are not build output"),
        )),
        (State::Unchecked(message), _) => Some(("kept".cyan().to_string(), message.clone())),
        _ => None,
    }
}

fn merged_detail(worktree: &Worktree, discarded: usize) -> String {
    let branch = worktree
        .branch
        .as_deref()
        .map_or("detached".to_owned(), |branch| format!("branch {branch}"));
    if discarded > 0 {
        format!("{branch}, merged ({discarded} deleted build-output path(s) discarded)")
    } else {
        format!("{branch}, merged and clean")
    }
}

fn state_code(state: &State) -> &'static str {
    match state {
        State::Main => "main",
        State::Locked => "locked",
        State::Current => "current",
        State::Missing => "missing",
        State::NotMerged => "not_merged",
        State::LocalChanges { .. } => "local_changes",
        State::Merged { .. } => "merged",
        State::Unchecked(_) => "unchecked",
    }
}

/// Builds the JSON document.
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
