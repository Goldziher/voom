//! The two renderers over one result set, in the same house style as `bazel::report` and
//! `git::report` (ADR 0007): an aligned em dash before a reason, colour as a second channel,
//! every state named in words, deterministic order. Always the explicit subcommand's full
//! listing — every job found, including the ones left alone, since there is no sweep to stay
//! quiet on the user's behalf.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use owo_colors::{OwoColorize, Style};
use serde_json::{Value, json};

use super::{ClaudePruneResult, Job, JobState, SCHEMA_VERSION};
use crate::delete::Outcome;

const GUTTER: &str = "  ";
const BECAUSE: &str = "— ";

mod palette {
    use owo_colors::Style;

    pub fn removed() -> Style {
        Style::new().green()
    }

    pub fn would_remove() -> Style {
        Style::new().yellow()
    }

    pub fn kept() -> Style {
        Style::new().cyan()
    }

    pub fn live() -> Style {
        Style::new().blue()
    }

    pub fn refused() -> Style {
        Style::new().red()
    }

    pub fn quiet() -> Style {
        Style::new().dimmed()
    }
}

struct Row {
    label: &'static str,
    style: Style,
    path: String,
    detail: Option<String>,
}

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

fn days(age: Duration) -> u64 {
    age.as_secs() / (24 * 60 * 60)
}

fn row(job: &Job, dry_run: bool, remove: bool, base: Option<&Path>) -> Row {
    let (label, style, detail) = match (&job.state, &job.outcome) {
        (JobState::Unproven, _) => ("kept", palette::kept(), Some("no readable state.json".to_owned())),
        (JobState::Live { session_id }, _) => (
            "kept",
            palette::live(),
            Some(format!("session {session_id} is running right now")),
        ),
        (JobState::TooRecent { age }, _) => (
            "kept",
            palette::kept(),
            Some(format!("quiet for only {} days", days(*age))),
        ),
        (JobState::Eligible { age }, Some(Outcome::Removed)) => (
            "removed",
            palette::removed(),
            Some(format!("quiet for {} days", days(*age))),
        ),
        (JobState::Eligible { age }, Some(Outcome::WouldRemove)) => (
            if dry_run { "would remove" } else { "removed" },
            palette::would_remove(),
            Some(format!("quiet for {} days", days(*age))),
        ),
        (JobState::Eligible { age }, Some(Outcome::Refused(refusal))) => (
            "refused",
            palette::refused(),
            Some(format!("{refusal} — quiet for {} days", days(*age))),
        ),
        (JobState::Eligible { age }, Some(Outcome::Failed(failure))) => (
            "failed",
            palette::refused(),
            Some(format!("{failure} — quiet for {} days", days(*age))),
        ),
        (JobState::Eligible { age }, Some(Outcome::PartiallyRemoved { failure, .. })) => (
            "partially removed",
            palette::refused(),
            Some(format!("{failure} — quiet for {} days", days(*age))),
        ),
        (JobState::Eligible { age }, None) if remove => (
            "kept",
            palette::kept(),
            Some(format!(
                "quiet for {} days, but its scratch is already gone",
                days(*age)
            )),
        ),
        (JobState::Eligible { age }, None) => (
            "eligible",
            palette::kept(),
            Some(format!(
                "quiet for {} days — rerun with --remove to reclaim it",
                days(*age)
            )),
        ),
    };
    Row {
        label,
        style,
        path: shorten(&job.path, base),
        detail,
    }
}

/// Renders the subcommand's human report.
///
/// # Errors
///
/// Propagates any write failure from `out`.
pub fn render_human(result: &ClaudePruneResult, out: &mut impl io::Write) -> io::Result<()> {
    let base = base_path(&result.roots);
    let rows: Vec<Row> = result
        .jobs
        .iter()
        .map(|job| row(job, result.dry_run, result.remove, base))
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

fn write_block(rows: &[Row], out: &mut impl io::Write) -> io::Result<()> {
    let label_width = rows.iter().map(|row| width_of(row.label)).max().unwrap_or(0);
    let path_width = rows
        .iter()
        .filter(|row| row.detail.is_some())
        .map(|row| width_of(&row.path))
        .max()
        .unwrap_or(0);

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

fn write_footer(result: &ClaudePruneResult, out: &mut impl io::Write) -> io::Result<()> {
    let totals = result.totals();
    let verb = match (result.remove, result.dry_run) {
        (false, _) => "found (rerun with --remove to reclaim any)",
        (true, true) => "would be reclaimed",
        (true, false) => "reclaimed",
    };
    writeln!(
        out,
        "{} {} found, {} {verb} ({}) in {:.2?}",
        totals.found.bold(),
        jobs_word(totals.found),
        totals.removed.bold(),
        humansize::format_size(totals.bytes, humansize::BINARY),
        result.elapsed
    )?;
    if totals.refused > 0 {
        writeln!(
            out,
            "{GUTTER}{}",
            format!("{} refused by a rail", totals.refused).style(palette::refused())
        )?;
    }
    Ok(())
}

fn jobs_word(count: usize) -> &'static str {
    if count == 1 { "job" } else { "jobs" }
}

fn width_of(text: &str) -> usize {
    text.chars().count()
}

fn write_fill(out: &mut impl io::Write, spaces: usize) -> io::Result<()> {
    write!(out, "{:spaces$}", "")
}

/// Builds the JSON document.
#[must_use]
pub fn document(result: &ClaudePruneResult) -> Value {
    let totals = result.totals();
    json!({
        "schema_version": SCHEMA_VERSION,
        "remove": result.remove,
        "dry_run": result.dry_run,
        "roots": result.roots.iter().map(|root| root.display().to_string()).collect::<Vec<_>>(),
        "jobs": result.jobs.iter().map(job_value).collect::<Vec<_>>(),
        "totals": {
            "found": totals.found,
            "removed": totals.removed,
            "refused": totals.refused,
            "kept": totals.kept,
            "bytes": totals.bytes,
        },
        "elapsed_ms": u64::try_from(result.elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

fn job_value(job: &Job) -> Value {
    let (state, detail) = match &job.state {
        JobState::Unproven => ("unproven", json!(null)),
        JobState::Live { session_id } => ("live", json!({ "session_id": session_id })),
        JobState::TooRecent { age } => ("too_recent", json!({ "age_days": days(*age) })),
        JobState::Eligible { age } => ("eligible", json!({ "age_days": days(*age) })),
    };
    json!({
        "path": job.path.display().to_string(),
        "state": state,
        "detail": detail,
        "bytes": job.bytes,
        "outcome": job.outcome.as_ref().map(outcome_code),
    })
}

fn outcome_code(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Removed => "removed",
        Outcome::WouldRemove => "would_remove",
        Outcome::Refused(_) => "refused",
        Outcome::Failed(_) => "failed",
        Outcome::PartiallyRemoved { .. } => "partially_removed",
    }
}

/// Renders the document as pretty-printed JSON.
///
/// # Errors
///
/// Propagates write and serialization failures from `out`.
pub fn render_json(result: &ClaudePruneResult, out: &mut impl io::Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, &document(result))?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(jobs: Vec<Job>, remove: bool, dry_run: bool) -> ClaudePruneResult {
        ClaudePruneResult {
            roots: vec![PathBuf::from("/home/dev/.claude/jobs")],
            jobs,
            remove,
            dry_run,
            elapsed: Duration::from_millis(20),
        }
    }

    fn eligible(path: &str, age_days: u64, outcome: Option<Outcome>) -> Job {
        Job {
            path: PathBuf::from(path),
            state: JobState::Eligible {
                age: Duration::from_secs(age_days * 86400),
            },
            bytes: outcome.is_some().then_some(4096),
            outcome,
        }
    }

    #[test]
    fn should_report_eligible_without_remove_and_say_how_to_reclaim_it() {
        let mut buffer = Vec::new();
        render_human(
            &result(vec![eligible("/home/dev/.claude/jobs/old", 30, None)], false, false),
            &mut buffer,
        )
        .expect("writing to a Vec cannot fail");
        let text = String::from_utf8(buffer).expect("the report is UTF-8");
        assert!(text.contains("eligible"), "{text}");
        assert!(text.contains("--remove"), "{text}");
    }

    #[test]
    fn should_name_a_live_session_and_never_call_it_removed() {
        let job = Job {
            path: PathBuf::from("/home/dev/.claude/jobs/live"),
            state: JobState::Live {
                session_id: "abc-123".to_owned(),
            },
            bytes: None,
            outcome: None,
        };
        let mut buffer = Vec::new();
        render_human(&result(vec![job], true, false), &mut buffer).expect("writing to a Vec cannot fail");
        let text = String::from_utf8(buffer).expect("the report is UTF-8");
        assert!(text.contains("is running right now"), "{text}");
        assert!(text.contains("kept"), "{text}");
    }

    #[test]
    fn should_write_valid_json_carrying_the_schema_version() {
        let mut buffer = Vec::new();
        render_json(&result(Vec::new(), false, false), &mut buffer).expect("writing to a Vec cannot fail");
        let parsed: Value = serde_json::from_slice(&buffer).expect("the output parses");
        assert_eq!(parsed["schema_version"], SCHEMA_VERSION);
        assert_eq!(parsed["remove"], false);
    }
}
