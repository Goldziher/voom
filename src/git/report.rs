//! The two renderers over one result set.
//!
//! Split from the rest of the module for the same reason `src/report/` is a module of its own:
//! rendering knows how wide a column is and which word names a state, and knows nothing about
//! subprocesses. Both renderers are fed from one [`GitPruneResult`], so the JSON cannot drift
//! from what the human saw.
//!
//! The house style is the sweep's (ADR 0007): blocks, an aligned em dash before a reason, colour
//! as a second channel and never the only one, every state named in words, and a deterministic
//! order.
//!
//! There are two audiences. `voom git-prune` asked, so it is told about every repository. A
//! sweep did not, so [`write_sweep_block`] prints only the repositories where something happened
//! — and nothing at all, not even a heading, when nothing did.

use std::io;
use std::path::{Path, PathBuf};

use owo_colors::{OwoColorize, Style};
use serde_json::{Value, json};

use super::{GitPruneResult, Repository, SCHEMA_VERSION, Status, Step, StepKind, StepOutcome};

/// The widest a path column is padded out to before its explanation, as in the sweep's report.
const MAX_PATH_COLUMN: usize = 40;

/// Spaces between two columns.
const GUTTER: &str = "  ";

/// Introduces the explanation that follows a path.
const BECAUSE: &str = "— ";

/// The palette, matching the sweep's report: colour is a second channel and never the only one.
mod palette {
    use owo_colors::Style;

    pub fn pruned() -> Style {
        Style::new().green()
    }

    pub fn would_prune() -> Style {
        Style::new().yellow()
    }

    /// A rail doing its job, which the sweep also prints in cyan.
    pub fn skipped() -> Style {
        Style::new().cyan()
    }

    pub fn failed() -> Style {
        Style::new().red()
    }

    pub fn quiet() -> Style {
        Style::new().dimmed()
    }
}

/// One rendered line, with every column held as plain text so the block can be measured.
struct Row {
    label: &'static str,
    style: Style,
    path: String,
    detail: Option<String>,
}

/// The directory the report prints paths relative to: the deepest one containing every root.
fn base_path(roots: &[PathBuf]) -> Option<&Path> {
    let first = roots.first()?;
    let base = first
        .ancestors()
        .find(|candidate| roots.iter().all(|root| root.starts_with(candidate)))?;
    (base.is_absolute() && base.parent().is_some()).then_some(base)
}

fn shorten(path: &Path, base: Option<&Path>) -> String {
    let Some(base) = base else {
        return path.display().to_string();
    };
    match path.strip_prefix(base) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Ok(relative) => relative.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

/// Everything the repository's steps had to say, in one line.
fn detail_of(repository: &Repository) -> Option<String> {
    if let Some(skipped) = &repository.skipped {
        return Some(skipped.to_string());
    }
    let parts: Vec<String> = repository
        .steps
        .iter()
        .filter(|step| step.kind != StepKind::RemoteList)
        .filter_map(step_detail)
        .collect();
    if parts.is_empty() {
        // A `remote` listing that failed is the only step the filter above drops, so a failure
        // there is named rather than left as a bare "failed".
        return repository
            .steps
            .iter()
            .find(|step| !step.succeeded())
            .map(|step| format!("{} failed", step.label()));
    }
    Some(parts.join("; "))
}

fn step_detail(step: &Step) -> Option<String> {
    match &step.outcome {
        StepOutcome::Succeeded { summary } if summary.is_empty() => None,
        StepOutcome::Succeeded { summary } => Some(summary.clone()),
        StepOutcome::Failed {
            code: Some(code),
            message,
        } => Some(format!("{} exited {code}: {message}", step.label())),
        StepOutcome::Failed { code: None, message } => Some(format!("{}: {message}", step.label())),
        StepOutcome::TimedOut => Some(format!("{} timed out and was killed", step.label())),
    }
}

fn row(repository: &Repository, dry_run: bool, base: Option<&Path>) -> Row {
    let (label, style) = match repository.status() {
        Status::Pruned if dry_run => ("would prune", palette::would_prune()),
        Status::Pruned => ("pruned", palette::pruned()),
        Status::Skipped => ("skipped", palette::skipped()),
        Status::Failed => ("failed", palette::failed()),
    };
    Row {
        label,
        style,
        path: shorten(&repository.path, base),
        detail: detail_of(repository),
    }
}

/// Renders the subcommand's human report, which names every repository it looked at.
///
/// # Errors
///
/// Propagates any write failure from `out`.
pub fn render_human(result: &GitPruneResult, out: &mut impl io::Write) -> io::Result<()> {
    let base = base_path(&result.roots);
    let rows: Vec<Row> = result
        .repositories
        .iter()
        .map(|repository| row(repository, result.dry_run, base))
        .collect();

    if let Some(base) = base.filter(|_| !rows.is_empty()) {
        writeln!(out, "{}", base.display().bold())?;
    }
    write_block(&rows, out)?;
    if !rows.is_empty() {
        writeln!(out)?;
    }
    write_footer(result, out)
}

/// Renders the git block for a *sweep's* report: the noteworthy repositories and nothing else.
///
/// `base` is the sweep's own base path, so the git rows line up with the artifact rows above
/// them rather than being shortened against a different directory.
///
/// Returns whether anything was written, so the caller can place its blank lines. Writes nothing
/// at all — not even a heading — when the sweep pruned nothing, which is the correct output for a
/// no-op.
///
/// # Errors
///
/// Propagates any write failure from `out`.
pub fn write_sweep_block(result: &GitPruneResult, base: Option<&Path>, out: &mut impl io::Write) -> io::Result<bool> {
    let noteworthy = result.noteworthy();
    if noteworthy.is_empty() {
        return Ok(false);
    }
    let rows: Vec<Row> = noteworthy
        .iter()
        .map(|repository| row(repository, result.dry_run, base))
        .collect();
    write_block(&rows, out)?;
    Ok(true)
}

fn write_block(rows: &[Row], out: &mut impl io::Write) -> io::Result<()> {
    let label_width = rows.iter().map(|row| width_of(row.label)).max().unwrap_or(0);
    // Only the rows carrying an explanation are measured: they are the only ones the column
    // exists to line up.
    let path_width = rows
        .iter()
        .filter(|row| row.detail.is_some())
        .map(|row| width_of(&row.path))
        .max()
        .unwrap_or(0)
        .min(MAX_PATH_COLUMN);

    for row in rows {
        write!(out, "{GUTTER}{}", row.style.style(row.label))?;
        write_fill(out, label_width.saturating_sub(width_of(row.label)))?;
        write!(out, "{GUTTER}{}", row.path)?;
        match &row.detail {
            None => writeln!(out)?,
            Some(detail) => {
                write_fill(out, path_width.saturating_sub(width_of(&row.path)))?;
                writeln!(
                    out,
                    "{GUTTER}{}",
                    format_args!("{BECAUSE}{detail}").style(palette::quiet())
                )?;
            }
        }
    }
    Ok(())
}

fn write_footer(result: &GitPruneResult, out: &mut impl io::Write) -> io::Result<()> {
    let totals = result.totals();
    let verb = if result.dry_run { "would be pruned" } else { "pruned" };
    writeln!(
        out,
        "{} {}, {} {verb} in {:.2?}",
        totals.repositories.bold(),
        repositories_word(totals.repositories),
        totals.pruned.bold(),
        result.elapsed
    )?;
    // Totals of different kinds get their own lines, the way the sweep's footer separates them.
    if totals.skipped > 0 {
        let text = format!("{} skipped", totals.skipped);
        writeln!(out, "{GUTTER}{}", text.style(palette::skipped()))?;
    }
    if totals.failed > 0 {
        let text = format!("{} failed", totals.failed);
        writeln!(out, "{GUTTER}{}", text.style(palette::failed()))?;
    }
    Ok(())
}

fn repositories_word(count: usize) -> &'static str {
    if count == 1 { "repository" } else { "repositories" }
}

/// The column count `text` occupies. Characters rather than bytes, which is right for the
/// non-Latin path components a filesystem tool meets.
fn width_of(text: &str) -> usize {
    text.chars().count()
}

fn write_fill(out: &mut impl io::Write, spaces: usize) -> io::Result<()> {
    write!(out, "{:spaces$}", "")
}

/// Builds the JSON document.
///
/// Exposed so tests can assert on structure rather than on text, and so the sweep's renderer can
/// embed it whole under its own `git` key.
#[must_use]
pub fn document(result: &GitPruneResult) -> Value {
    let totals = result.totals();
    json!({
        "schema_version": SCHEMA_VERSION,
        "dry_run": result.dry_run,
        "roots": result.roots.iter().map(|root| root.display().to_string()).collect::<Vec<_>>(),
        "repositories": result
            .repositories
            .iter()
            .map(|repository| repository_value(repository, result.dry_run))
            .collect::<Vec<_>>(),
        "totals": {
            "repositories": totals.repositories,
            "pruned": totals.pruned,
            "skipped": totals.skipped,
            "failed": totals.failed,
        },
        "elapsed_ms": u64::try_from(result.elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

fn repository_value(repository: &Repository, dry_run: bool) -> Value {
    let status = match repository.status() {
        Status::Pruned if dry_run => "would_prune",
        Status::Pruned => "pruned",
        Status::Skipped => "skipped",
        Status::Failed => "failed",
    };
    json!({
        "path": repository.path.display().to_string(),
        "status": status,
        "skipped": repository.skipped.as_ref().map(|skipped| json!({
            "reason": skipped.code(),
            "detail": skipped.to_string(),
        })),
        "steps": repository
            .steps
            .iter()
            .filter(|step| step.kind != StepKind::RemoteList || !step.succeeded())
            .map(step_value)
            .collect::<Vec<_>>(),
    })
}

fn step_value(step: &Step) -> Value {
    let outcome = match &step.outcome {
        StepOutcome::Succeeded { summary } => json!({ "status": "succeeded", "detail": summary }),
        StepOutcome::Failed { code, message } => json!({
            "status": "failed",
            "exit_code": code,
            "detail": message,
        }),
        StepOutcome::TimedOut => json!({
            "status": "timed_out",
            "detail": "killed after the repository timeout",
        }),
    };
    json!({
        "step": step.kind.code(),
        "remote": step.remote,
        // The argv voom actually built. A reader checking that a sweep made no network call reads
        // this rather than trusting the documentation.
        "command": step.command,
        "outcome": outcome,
    })
}

/// Renders the document as pretty-printed JSON.
///
/// # Errors
///
/// Propagates write and serialization failures from `out`.
pub fn render_json(result: &GitPruneResult, out: &mut impl io::Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, &document(result))?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Skipped;
    use std::time::Duration;

    fn step(kind: StepKind, outcome: StepOutcome) -> Step {
        Step {
            kind,
            remote: None,
            command: vec!["git".to_owned(), kind.code().to_owned()],
            outcome,
        }
    }

    fn succeeded(kind: StepKind, summary: &str) -> Step {
        step(
            kind,
            StepOutcome::Succeeded {
                summary: summary.to_owned(),
            },
        )
    }

    fn result(repositories: Vec<Repository>, dry_run: bool) -> GitPruneResult {
        GitPruneResult {
            roots: vec![PathBuf::from("/projects")],
            repositories,
            dry_run,
            elapsed: Duration::from_millis(120),
        }
    }

    fn repository(path: &str, steps: Vec<Step>) -> Repository {
        Repository {
            path: PathBuf::from(path),
            skipped: None,
            steps,
        }
    }

    fn rendered(result: &GitPruneResult) -> String {
        let mut buffer = Vec::new();
        render_human(result, &mut buffer).expect("writing to a Vec cannot fail");
        String::from_utf8(buffer).expect("the report is UTF-8")
    }

    /// The rule the sweep's report depends on: a run where `gc --auto` decided against repacking
    /// and no worktree was stale says nothing at all.
    #[test]
    fn should_write_nothing_into_a_sweep_when_nothing_happened() {
        let quiet = result(
            vec![repository(
                "/projects/api",
                vec![succeeded(StepKind::WorktreePrune, ""), succeeded(StepKind::Gc, "")],
            )],
            false,
        );

        let mut buffer = Vec::new();
        let wrote = write_sweep_block(&quiet, None, &mut buffer).expect("writing to a Vec cannot fail");

        assert!(!wrote, "a no-op is silent");
        assert!(buffer.is_empty(), "{}", String::from_utf8_lossy(&buffer));
    }

    #[test]
    fn should_write_a_sweep_line_for_a_repository_where_something_happened() {
        let pruned = result(
            vec![
                repository(
                    "/projects/api",
                    vec![
                        succeeded(StepKind::WorktreePrune, "1 stale worktree pruned"),
                        succeeded(StepKind::Gc, ""),
                    ],
                ),
                repository(
                    "/projects/quiet",
                    vec![succeeded(StepKind::WorktreePrune, ""), succeeded(StepKind::Gc, "")],
                ),
            ],
            false,
        );

        let mut buffer = Vec::new();
        let wrote = write_sweep_block(&pruned, Some(Path::new("/projects")), &mut buffer).expect("a write");
        let text = String::from_utf8(buffer).expect("the report is UTF-8");

        assert!(wrote);
        assert!(text.contains("api"), "{text}");
        assert!(text.contains("1 stale worktree pruned"), "{text}");
        assert!(
            !text.contains("quiet"),
            "a repository with nothing to say gets no line: {text}"
        );
    }

    /// The explicit subcommand reports every repository, including the quiet ones — there the
    /// user asked.
    #[test]
    fn should_name_every_repository_in_the_subcommands_report() {
        let text = rendered(&result(
            vec![repository(
                "/projects/quiet",
                vec![succeeded(StepKind::WorktreePrune, ""), succeeded(StepKind::Gc, "")],
            )],
            false,
        ));

        assert!(text.contains("quiet"), "{text}");
        // The counts are bold, so the escapes sit between them and the words they qualify —
        // asserting on the words is what survives that without hard-coding the palette.
        assert!(text.contains("repository, "), "{text}");
        assert!(text.contains(" pruned in "), "{text}");
    }

    /// Every state is named in words, so the report survives `NO_COLOR` and a pipe to a file.
    #[test]
    fn should_name_a_skip_and_its_reason_in_words() {
        let mut skipped = repository("/projects/halfway", Vec::new());
        skipped.skipped = Some(Skipped::InProgress {
            marker: ".git/rebase-merge".to_owned(),
        });

        let text = rendered(&result(vec![skipped], false));

        assert!(text.contains("skipped"), "{text}");
        assert!(
            text.contains("a git operation is in progress (.git/rebase-merge)"),
            "{text}"
        );
        assert!(text.contains("1 skipped"), "the footer counts it: {text}");
    }

    #[test]
    fn should_name_a_failure_and_what_git_said() {
        let failed = repository(
            "/projects/broken",
            vec![step(
                StepKind::WorktreePrune,
                StepOutcome::Failed {
                    code: Some(128),
                    message: "fatal: bad config line 1".to_owned(),
                },
            )],
        );

        let text = rendered(&result(vec![failed], false));

        assert!(text.contains("failed"), "{text}");
        assert!(
            text.contains("worktree prune exited 128: fatal: bad config line 1"),
            "{text}"
        );
    }

    #[test]
    fn should_carry_the_schema_version_and_the_argv_it_ran() {
        let document = document(&result(
            vec![repository(
                "/projects/api",
                vec![succeeded(StepKind::WorktreePrune, "1 stale worktree pruned")],
            )],
            false,
        ));

        assert_eq!(document["schema_version"], SCHEMA_VERSION);
        assert_eq!(document["repositories"][0]["status"], "pruned");
        assert_eq!(document["repositories"][0]["steps"][0]["step"], "worktree_prune");
        assert_eq!(document["totals"]["pruned"], 1);
    }

    #[test]
    fn should_mark_a_dry_run_as_would_prune() {
        let document = document(&result(
            vec![repository(
                "/projects/api",
                vec![succeeded(StepKind::WorktreePrune, "")],
            )],
            true,
        ));
        assert_eq!(document["repositories"][0]["status"], "would_prune");
        assert_eq!(document["dry_run"], true);
    }

    #[test]
    fn should_write_valid_json() {
        let mut buffer = Vec::new();
        render_json(&result(Vec::new(), true), &mut buffer).expect("writing to a Vec cannot fail");
        let parsed: Value = serde_json::from_slice(&buffer).expect("the output parses");
        assert_eq!(parsed["totals"]["repositories"], 0);
    }
}
