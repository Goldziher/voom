//! The two renderers over one result, in the same house style as `bazel::report` and
//! `claude::report` (ADR 0007): an em dash before a reason, colour as a second channel, every
//! state named in words, deterministic order.
//!
//! Sizes are decimal (`1.5 GB`), unlike the binary units the filesystem reports use, because every
//! figure here originates in docker's own decimal output and is meant to be compared with
//! `docker system df`.

use std::io;
use std::time::Duration;

use humansize::{DECIMAL, format_size};
use owo_colors::{OwoColorize, Style};
use serde_json::{Value, json};

use super::plan::{LaneKind, VolumeScope};
use super::{DockerPruneResult, Lane, LaneOutcome, SCHEMA_VERSION, Trim, TrimOutcome};

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
    subject: String,
    detail: String,
}

fn size(bytes: u64) -> String {
    format_size(bytes, DECIMAL)
}

fn days(age: Duration) -> String {
    let hours = age.as_secs() / 3600;
    if hours.is_multiple_of(24) {
        format!("{}d", hours / 24)
    } else {
        format!("{hours}h")
    }
}

fn noun(kind: LaneKind, count: usize) -> &'static str {
    match (kind, count) {
        (LaneKind::Images, 1) => "image",
        (LaneKind::Images, _) => "images",
        (LaneKind::BuildCache, 1) => "cache record",
        (LaneKind::BuildCache, _) => "cache records",
        (LaneKind::Volumes, 1) => "volume",
        (LaneKind::Volumes, _) => "volumes",
    }
}

fn lane_row(lane: &Lane, result: &DockerPruneResult) -> Row {
    let kind = lane.plan.kind;
    let age = days(result.max_age);
    let scope = match kind {
        LaneKind::Images => format!("dangling, or unused for over {age}"),
        LaneKind::BuildCache => format!("idle for over {age}"),
        LaneKind::Volumes => format!(
            "{}, mounted by no container",
            result.volumes.map_or("none", |scope| scope.label())
        ),
    };
    let counted = format!(
        "{} {}, {}",
        lane.plan.candidates,
        noun(kind, lane.plan.candidates),
        size(lane.bytes())
    );
    let (label, style, detail) = match &lane.outcome {
        LaneOutcome::WouldReclaim => ("would reclaim", palette::would_remove(), format!("{counted}: {scope}")),
        LaneOutcome::Reclaimed { bytes: Some(_) } => ("reclaimed", palette::removed(), format!("{counted}: {scope}")),
        LaneOutcome::Reclaimed { bytes: None } => (
            "pruned",
            palette::removed(),
            format!(
                "{} {} matched: {scope}; docker reported no size",
                lane.plan.candidates,
                noun(kind, lane.plan.candidates)
            ),
        ),
        LaneOutcome::Skipped { reason } => ("skipped", palette::kept(), reason.clone()),
        LaneOutcome::Failed { message } => ("failed", palette::refused(), message.clone()),
    };
    Row {
        label,
        style,
        subject: kind.label().to_owned(),
        detail,
    }
}

fn trim_row(trim: &Trim) -> Row {
    let (label, style, detail) = match &trim.outcome {
        TrimOutcome::Withheld { host_bytes } => (
            "would trim",
            palette::would_remove(),
            format!(
                "`{}`; the host disk image holds {} now",
                trim.command.join(" "),
                host_bytes.map_or_else(|| "an unmeasured amount".to_owned(), size)
            ),
        ),
        TrimOutcome::Trimmed { before, after } => (
            "trimmed",
            palette::removed(),
            match (before, after, trim.outcome.returned()) {
                (Some(before), Some(after), Some(returned)) => {
                    format!(
                        "host disk image {} -> {}, {} returned",
                        size(*before),
                        size(*after),
                        size(returned)
                    )
                }
                _ => "host disk image not found, so the bytes returned are unmeasured".to_owned(),
            },
        ),
        TrimOutcome::Failed { message } => ("failed", palette::refused(), message.clone()),
    };
    Row {
        label,
        style,
        subject: "vm disk".to_owned(),
        detail,
    }
}

/// Renders the subcommand's human report. A skipped run is one line.
///
/// # Errors
///
/// Propagates any write failure from `out`.
pub fn render_human(result: &DockerPruneResult, out: &mut impl io::Write) -> io::Result<()> {
    if let Some(skip) = &result.skipped {
        return writeln!(out, "voom: docker: {} (skipped)", skip.reason);
    }

    let mut rows: Vec<Row> = result.lanes.iter().map(|lane| lane_row(lane, result)).collect();
    rows.extend(result.trim.as_ref().map(trim_row));

    let heading = result.backend.as_deref().unwrap_or("docker");
    writeln!(out, "{}", format!("docker — {heading}").bold())?;
    write_block(&rows, out)?;
    writeln!(out)?;
    write_footer(result, out)
}

fn write_block(rows: &[Row], out: &mut impl io::Write) -> io::Result<()> {
    let label_width = rows.iter().map(|row| row.label.chars().count()).max().unwrap_or(0);
    let subject_width = rows.iter().map(|row| row.subject.chars().count()).max().unwrap_or(0);
    for row in rows {
        writeln!(
            out,
            "{GUTTER}{label:<label_width$}{GUTTER}{subject:<subject_width$}{GUTTER}{detail}",
            label = row.style.style(row.label),
            subject = row.subject,
            detail = format_args!("{BECAUSE}{}", row.detail).style(palette::quiet()),
        )?;
    }
    Ok(())
}

fn write_footer(result: &DockerPruneResult, out: &mut impl io::Write) -> io::Result<()> {
    let totals = result.totals();
    let verb = if result.dry_run {
        "would be reclaimed"
    } else {
        "reclaimed"
    };
    write!(
        out,
        "{} matched, {} {verb} in the VM",
        totals.candidates.bold(),
        size(totals.bytes)
    )?;
    if let Some(returned) = totals.host_bytes_returned {
        write!(out, ", {} returned to the host", size(returned))?;
    }
    writeln!(out, " in {:.2?}", result.elapsed)?;
    if totals.failed > 0 {
        writeln!(
            out,
            "{GUTTER}{}",
            format!("{} lane(s) failed; the sweep is unaffected", totals.failed).style(palette::refused())
        )?;
    }
    Ok(())
}

/// Builds the JSON document.
#[must_use]
pub fn document(result: &DockerPruneResult) -> Value {
    let totals = result.totals();
    json!({
        "schema_version": SCHEMA_VERSION,
        "dry_run": result.dry_run,
        "status": if result.skipped.is_some() { "skipped" } else { "ready" },
        "skipped_reason": result.skipped.as_ref().map(|skip| skip.reason.as_str()),
        "endpoint": result.endpoint,
        "backend": result.backend,
        "max_age_secs": result.max_age.as_secs(),
        "volumes": result.volumes.map(VolumeScope::label),
        "lanes": result.lanes.iter().map(lane_value).collect::<Vec<_>>(),
        "trim": result.trim.as_ref().map(trim_value),
        "totals": {
            "candidates": totals.candidates,
            "bytes": totals.bytes,
            "failed": totals.failed,
            "host_bytes_returned": totals.host_bytes_returned,
        },
        "elapsed_ms": u64::try_from(result.elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

fn lane_value(lane: &Lane) -> Value {
    let (outcome, reclaimed, message) = match &lane.outcome {
        LaneOutcome::WouldReclaim => ("would_reclaim", None, None),
        LaneOutcome::Reclaimed { bytes } => ("reclaimed", *bytes, None),
        LaneOutcome::Skipped { reason } => ("skipped", None, Some(reason.as_str())),
        LaneOutcome::Failed { message } => ("failed", None, Some(message.as_str())),
    };
    json!({
        "kind": match lane.plan.kind {
            LaneKind::Images => "images",
            LaneKind::BuildCache => "build_cache",
            LaneKind::Volumes => "volumes",
        },
        "candidates": lane.plan.candidates,
        "estimated_bytes": lane.plan.estimated_bytes,
        "reclaimed_bytes": reclaimed,
        "outcome": outcome,
        "message": message,
        "commands": lane.plan.commands.iter().map(|command| format!("docker {}", command.join(" "))).collect::<Vec<_>>(),
    })
}

fn trim_value(trim: &Trim) -> Value {
    let (outcome, before, after, message) = match &trim.outcome {
        TrimOutcome::Withheld { host_bytes } => ("would_trim", *host_bytes, None, None),
        TrimOutcome::Trimmed { before, after } => ("trimmed", *before, *after, None),
        TrimOutcome::Failed { message } => ("failed", None, None, Some(message.as_str())),
    };
    json!({
        "backend": trim.backend,
        "command": trim.command.join(" "),
        "outcome": outcome,
        "host_bytes_before": before,
        "host_bytes_after": after,
        "host_bytes_returned": trim.outcome.returned(),
        "message": message,
    })
}

/// Renders the document as pretty-printed JSON.
///
/// # Errors
///
/// Propagates write and serialization failures from `out`.
pub fn render_json(result: &DockerPruneResult, out: &mut impl io::Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, &document(result))?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use super::super::Skip;
    use super::super::plan::LanePlan;
    use super::*;

    fn lane(kind: LaneKind, candidates: usize, estimated: u64, outcome: LaneOutcome) -> Lane {
        Lane {
            plan: LanePlan {
                kind,
                candidates,
                estimated_bytes: estimated,
                commands: vec![vec!["image".to_owned(), "prune".to_owned()]],
                skipped: None,
            },
            outcome,
        }
    }

    fn result(lanes: Vec<Lane>, trim: Option<Trim>, dry_run: bool) -> DockerPruneResult {
        DockerPruneResult {
            dry_run,
            max_age: Duration::from_secs(7 * 86_400),
            volumes: Some(VolumeScope::Anonymous),
            skipped: None,
            endpoint: Some("unix:///home/dev/.colima/default/docker.sock".to_owned()),
            backend: Some("colima (profile default)".to_owned()),
            lanes,
            trim,
            elapsed: Duration::from_millis(1500),
        }
    }

    fn text(result: &DockerPruneResult) -> String {
        let mut buffer = Vec::new();
        render_human(result, &mut buffer).expect("writing to a Vec cannot fail");
        String::from_utf8(buffer).expect("the report is UTF-8")
    }

    #[test]
    fn should_report_a_dry_run_with_the_estimate_and_the_withheld_trim() {
        let trim = Trim {
            backend: "colima (profile default)".to_owned(),
            command: vec!["colima".to_owned(), "ssh".to_owned()],
            outcome: TrimOutcome::Withheld {
                host_bytes: Some(120_000_000_000),
            },
        };
        let report = text(&result(
            vec![lane(LaneKind::Images, 2, 1_600_000_000, LaneOutcome::WouldReclaim)],
            Some(trim),
            true,
        ));
        assert!(report.contains("would reclaim"), "{report}");
        assert!(report.contains("2 images, 1.60 GB"), "{report}");
        assert!(report.contains("would trim"), "{report}");
        assert!(report.contains("would be reclaimed in the VM"), "{report}");
    }

    #[test]
    fn should_report_docker_bytes_and_the_host_bytes_returned_after_a_real_run() {
        let trim = Trim {
            backend: "colima (profile default)".to_owned(),
            command: vec!["colima".to_owned()],
            outcome: TrimOutcome::Trimmed {
                before: Some(120_000_000_000),
                after: Some(80_000_000_000),
            },
        };
        let report = text(&result(
            vec![lane(
                LaneKind::Images,
                2,
                1_600_000_000,
                LaneOutcome::Reclaimed {
                    bytes: Some(1_400_000_000),
                },
            )],
            Some(trim),
            false,
        ));
        assert!(
            report.contains("1 images") || report.contains("2 images, 1.40 GB"),
            "{report}"
        );
        assert!(report.contains("40 GB returned"), "{report}");
        assert!(report.contains("returned to the host"), "{report}");
    }

    #[test]
    fn should_say_one_line_when_docker_was_skipped() {
        let mut skipped = result(Vec::new(), None, false);
        skipped.skipped = Some(Skip {
            reason: "the docker daemon is not running".to_owned(),
        });
        assert_eq!(
            text(&skipped),
            "voom: docker: the docker daemon is not running (skipped)\n"
        );
    }

    #[test]
    fn should_write_valid_json_carrying_the_schema_version_and_skip_reason() {
        let mut skipped = result(Vec::new(), None, true);
        skipped.skipped = Some(Skip {
            reason: "`docker` is not installed".to_owned(),
        });
        let mut buffer = Vec::new();
        render_json(&skipped, &mut buffer).expect("writing to a Vec cannot fail");
        let parsed: Value = serde_json::from_slice(&buffer).expect("the output parses");
        assert_eq!(parsed["schema_version"], SCHEMA_VERSION);
        assert_eq!(parsed["status"], "skipped");
        assert_eq!(parsed["skipped_reason"], "`docker` is not installed");
    }

    #[test]
    fn should_name_each_lane_command_in_json() {
        let report = document(&result(
            vec![lane(LaneKind::Images, 1, 5, LaneOutcome::WouldReclaim)],
            None,
            true,
        ));
        assert_eq!(report["lanes"][0]["commands"][0], "docker image prune");
        assert_eq!(report["lanes"][0]["outcome"], "would_reclaim");
        assert_eq!(report["totals"]["bytes"], 5);
    }
}
