//! The two renderers over one result set.
//!
//! Split from the rest of the module for the same reason `git::report` is: rendering knows how
//! wide a column is and which word names a state, and knows nothing about markers or rails.
//!
//! The house style is the sweep's (ADR 0007): an aligned em dash before a reason, colour as a
//! second channel and never the only one, every state named in words, and a deterministic
//! order. `voom bazel-prune` names every output base it found, including the ones it left
//! alone — there is no sweep-integrated form to stay quiet on the user's behalf, since this is
//! always the explicit subcommand (`adrs/0014-bazel-output-base-housekeeping.md`).

use std::io;
use std::path::{Path, PathBuf};

use owo_colors::{OwoColorize, Style};
use serde_json::{Value, json};

use super::{BazelPruneResult, OutputBase, OutputBaseState, SCHEMA_VERSION};
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

fn row(base: &OutputBase, dry_run: bool, root_base: Option<&Path>) -> Row {
    let (label, style, detail) = match (&base.state, &base.outcome) {
        (OutputBaseState::Orphaned { owner }, Some(Outcome::Removed)) => {
            (removed_label(false), palette::removed(), Some(owned_detail(owner)))
        }
        (OutputBaseState::Orphaned { owner }, Some(Outcome::WouldRemove)) => (
            removed_label(dry_run),
            palette::would_remove(),
            Some(owned_detail(owner)),
        ),
        (OutputBaseState::Orphaned { owner }, Some(Outcome::Refused(refusal))) => (
            "refused",
            palette::refused(),
            Some(format!("{refusal} — owner {} is gone", owner.display())),
        ),
        (OutputBaseState::Orphaned { owner }, Some(Outcome::Failed(failure))) => (
            "failed",
            palette::refused(),
            Some(format!("{failure} — owner {} is gone", owner.display())),
        ),
        (OutputBaseState::Orphaned { owner }, Some(Outcome::PartiallyRemoved { failure, .. })) => (
            "partially removed",
            palette::refused(),
            Some(format!("{failure} — owner {} is gone", owner.display())),
        ),
        (OutputBaseState::Orphaned { owner }, None) => (
            "kept",
            palette::kept(),
            Some(format!("owner {} is gone, but nothing was attempted", owner.display())),
        ),
        (OutputBaseState::Owned { owner }, _) => ("kept", palette::kept(), Some(owned_detail(owner))),
        (OutputBaseState::Unproven, _) => ("kept", palette::kept(), Some("no owner marker found".to_owned())),
    };
    Row {
        label,
        style,
        path: shorten(&base.path, root_base),
        detail,
    }
}

fn removed_label(dry_run: bool) -> &'static str {
    if dry_run { "would remove" } else { "removed" }
}

fn owned_detail(owner: &Path) -> String {
    format!("owner {}", owner.display())
}

/// Renders the subcommand's human report.
///
/// # Errors
///
/// Propagates any write failure from `out`.
pub fn render_human(result: &BazelPruneResult, out: &mut impl io::Write) -> io::Result<()> {
    let base = base_path(&result.roots);
    let rows: Vec<Row> = result
        .output_bases
        .iter()
        .map(|output_base| row(output_base, result.dry_run, base))
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

fn write_footer(result: &BazelPruneResult, out: &mut impl io::Write) -> io::Result<()> {
    let totals = result.totals();
    let verb = if result.dry_run {
        "would be reclaimed"
    } else {
        "reclaimed"
    };
    writeln!(
        out,
        "{} output {}, {} {verb} ({}) in {:.2?}",
        totals.found.bold(),
        bases_word(totals.found),
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

fn bases_word(count: usize) -> &'static str {
    if count == 1 { "base" } else { "bases" }
}

fn width_of(text: &str) -> usize {
    text.chars().count()
}

fn write_fill(out: &mut impl io::Write, spaces: usize) -> io::Result<()> {
    write!(out, "{:spaces$}", "")
}

/// Builds the JSON document.
#[must_use]
pub fn document(result: &BazelPruneResult) -> Value {
    let totals = result.totals();
    json!({
        "schema_version": SCHEMA_VERSION,
        "dry_run": result.dry_run,
        "roots": result.roots.iter().map(|root| root.display().to_string()).collect::<Vec<_>>(),
        "output_bases": result.output_bases.iter().map(output_base_value).collect::<Vec<_>>(),
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

fn output_base_value(base: &OutputBase) -> Value {
    let (state, owner) = match &base.state {
        OutputBaseState::Unproven => ("unproven", None),
        OutputBaseState::Owned { owner } => ("owned", Some(owner)),
        OutputBaseState::Orphaned { owner } => ("orphaned", Some(owner)),
    };
    json!({
        "path": base.path.display().to_string(),
        "state": state,
        "owner": owner.map(|owner| owner.display().to_string()),
        "bytes": base.bytes,
        "outcome": base.outcome.as_ref().map(outcome_code),
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
pub fn render_json(result: &BazelPruneResult, out: &mut impl io::Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, &document(result))?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn result(output_bases: Vec<OutputBase>, dry_run: bool) -> BazelPruneResult {
        BazelPruneResult {
            roots: vec![PathBuf::from("/var/tmp/_bazel_dev")],
            output_bases,
            dry_run,
            elapsed: Duration::from_millis(50),
        }
    }

    fn orphaned(path: &str, owner: &str, outcome: Outcome) -> OutputBase {
        OutputBase {
            path: PathBuf::from(path),
            state: OutputBaseState::Orphaned {
                owner: PathBuf::from(owner),
            },
            bytes: Some(512),
            outcome: Some(outcome),
        }
    }

    #[test]
    fn should_name_a_removed_orphan_and_its_gone_owner() {
        let mut buffer = Vec::new();
        render_human(
            &result(
                vec![orphaned("/var/tmp/_bazel_dev/abc", "/workspace/gone", Outcome::Removed)],
                false,
            ),
            &mut buffer,
        )
        .expect("writing to a Vec cannot fail");
        let text = String::from_utf8(buffer).expect("the report is UTF-8");
        assert!(text.contains("removed"), "{text}");
        assert!(text.contains("owner /workspace/gone"), "{text}");
    }

    #[test]
    fn should_write_valid_json_carrying_the_schema_version() {
        let mut buffer = Vec::new();
        render_json(&result(Vec::new(), true), &mut buffer).expect("writing to a Vec cannot fail");
        let parsed: Value = serde_json::from_slice(&buffer).expect("the output parses");
        assert_eq!(parsed["schema_version"], SCHEMA_VERSION);
        assert_eq!(parsed["dry_run"], true);
    }

    #[test]
    fn should_mark_a_dry_run_removal_as_would_remove() {
        let document = document(&result(
            vec![orphaned(
                "/var/tmp/_bazel_dev/abc",
                "/workspace/gone",
                Outcome::WouldRemove,
            )],
            true,
        ));
        assert_eq!(document["output_bases"][0]["outcome"], "would_remove");
        assert_eq!(document["totals"]["removed"], 1);
    }
}
