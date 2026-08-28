//! The machine-readable renderer.
//!
//! One object with a stable, versioned shape. The document is built explicitly here rather than
//! derived from the internal types, so the wire format is visible in one place and cannot
//! change as a side effect of refactoring a struct somewhere else.
//!
//! Reason codes are enum-like strings rather than prose, so a hook can branch on them
//! (ADR 0007). They come from [`crate::classify::SkipReason::code`] and
//! [`crate::delete::Refusal::code`], which are the same values the human renderer explains.

use std::io;

use serde_json::{Value, json};

use super::{Entry, RunResult, SCHEMA_VERSION};
use crate::classify::Provenance;
use crate::delete::Outcome;

fn outcome_value(outcome: &Outcome) -> Value {
    match outcome {
        Outcome::Removed => json!({ "status": "removed" }),
        Outcome::WouldRemove => json!({ "status": "would_remove" }),
        Outcome::Refused(refusal) => json!({
            "status": "refused",
            "reason": refusal.code(),
            "detail": refusal.to_string(),
        }),
        Outcome::Failed(message) => json!({
            "status": "failed",
            "reason": "removal_failed",
            "detail": message,
        }),
    }
}

fn entry_value(entry: &Entry) -> Value {
    json!({
        "path": entry.path.display().to_string(),
        "ecosystem": entry.ecosystem(),
        "artifact": entry.artifact(),
        "bytes": entry.bytes,
        // `source` is the field a consumer branches on. `ecosystem`, `artifact` and
        // `marker_dir` are null for an unanchored removal, and a hook that treats a null
        // ecosystem as "unknown" rather than "nothing proved this" would be reading it wrong.
        "source": match &entry.finding.provenance {
            Provenance::Anchored { .. } => "marker",
            Provenance::Included { .. } => "config-include",
        },
        "marker_dir": match &entry.finding.provenance {
            Provenance::Anchored { marker_dir, .. } => Some(marker_dir.display().to_string()),
            Provenance::Included { .. } => None,
        },
        "include_pattern": match &entry.finding.provenance {
            Provenance::Anchored { .. } => None,
            Provenance::Included { pattern } => Some(pattern.as_str()),
        },
        "outcome": outcome_value(&entry.outcome),
    })
}

/// Builds the document. Exposed so tests can assert on structure rather than on text.
#[must_use]
pub fn document(result: &RunResult) -> Value {
    let totals = result.totals();
    json!({
        "schema_version": SCHEMA_VERSION,
        "dry_run": result.dry_run,
        "roots": result.roots.iter().map(|root| root.display().to_string()).collect::<Vec<_>>(),
        "artifacts": result.entries.iter().map(entry_value).collect::<Vec<_>>(),
        "skipped": result
            .skip_reasons()
            .map(|(path, reason)| json!({
                "path": path.display().to_string(),
                "reason": reason.code(),
                "detail": reason.to_string(),
            }))
            .collect::<Vec<_>>(),
        "walk_errors": result
            .failures
            .iter()
            .map(|failure| json!({
                "path": failure.path.as_ref().map(|path| path.display().to_string()),
                "detail": failure.message,
                "transient": failure.transient,
            }))
            .collect::<Vec<_>>(),
        "totals": {
            "reclaimed": totals.reclaimed,
            "bytes": totals.bytes,
            "skipped": totals.skipped,
            "refused": totals.refused,
            "failed": totals.failed,
        },
        "elapsed_ms": u64::try_from(result.elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

/// Renders the document as pretty-printed JSON on one line-delimited object.
///
/// # Errors
///
/// Propagates write failures and serialization failures from `out`.
pub fn render(result: &RunResult, out: &mut impl io::Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, &document(result))?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::SkipReason;
    use crate::delete::Refusal;
    use crate::report::fixtures::{entry, result};
    use crate::scan::Skipped;

    fn document_for_tests() -> Value {
        let mut result = result(
            vec![
                entry("/projects/api/target", "rust.target", 4_200_000_000, Outcome::Removed),
                entry(
                    "/projects/link/target",
                    "rust.target",
                    0,
                    Outcome::Refused(Refusal::Symlink),
                ),
                entry(
                    "/projects/locked/obj",
                    "dotnet.obj",
                    900,
                    Outcome::Failed("removing `/projects/locked/obj`: Permission denied".to_owned()),
                ),
            ],
            false,
        );
        result.skipped_count = 1;
        result.skips = vec![Skipped {
            path: std::path::PathBuf::from("/projects/data/target"),
            reason: SkipReason::NoMarker {
                ecosystem: "rust",
                markers: &["Cargo.toml"],
            },
        }];
        result.sort();
        document(&result)
    }

    #[test]
    fn should_render_the_documented_shape() {
        insta::assert_json_snapshot!(document_for_tests());
    }

    /// A hook reading this has to be able to tell a proven removal from one the user's config
    /// asked for outright, so `source` is the field it branches on.
    #[test]
    fn should_mark_an_unanchored_removal_as_config_sourced() {
        let result = crate::report::fixtures::result(
            vec![crate::report::fixtures::included(
                "/scratch/build-junk",
                "~/scratch/build-junk",
                1024,
                Outcome::Removed,
            )],
            false,
        );

        let artifact = document(&result)["artifacts"][0].clone();

        assert_eq!(artifact["source"], serde_json::json!("config-include"));
        assert_eq!(artifact["ecosystem"], Value::Null);
        assert_eq!(artifact["artifact"], Value::Null);
        assert_eq!(artifact["marker_dir"], Value::Null);
        assert_eq!(artifact["include_pattern"], serde_json::json!("~/scratch/build-junk"));
    }

    /// A machine consumer has no `--verbose` to reach for, so JSON always carries every walk
    /// error — but it has to say which of them are worth alerting on.
    #[test]
    fn should_mark_which_walk_errors_were_only_interruptions() {
        let mut result = crate::report::fixtures::result(Vec::new(), true);
        result.failures = vec![
            crate::scan::WalkFailure {
                path: Some(std::path::PathBuf::from("/projects/locked")),
                message: "Permission denied (os error 13)".to_owned(),
                transient: false,
            },
            crate::scan::WalkFailure {
                path: Some(std::path::PathBuf::from("/projects/containers")),
                message: "Interrupted system call (os error 4)".to_owned(),
                transient: true,
            },
        ];

        let errors = document(&result)["walk_errors"].clone();
        assert_eq!(errors[0]["transient"], serde_json::json!(false));
        assert_eq!(errors[1]["transient"], serde_json::json!(true));
    }

    #[test]
    fn should_carry_the_schema_version() {
        assert_eq!(document_for_tests()["schema_version"], SCHEMA_VERSION);
    }

    /// Hooks branch on these, so they are prose-free and stable.
    #[test]
    fn should_expose_enum_like_reason_codes() {
        let document = document_for_tests();
        assert_eq!(document["artifacts"][1]["outcome"]["reason"], "symlink");
        assert_eq!(document["artifacts"][2]["outcome"]["reason"], "removal_failed");
        assert_eq!(document["skipped"][0]["reason"], "no_marker");
    }

    #[test]
    fn should_agree_with_the_human_totals() {
        let document = document_for_tests();
        assert_eq!(document["totals"]["reclaimed"], 1);
        assert_eq!(document["totals"]["bytes"], 4_200_000_000_u64);
        assert_eq!(document["totals"]["refused"], 1);
        assert_eq!(document["totals"]["failed"], 1);
    }

    #[test]
    fn should_write_valid_json() {
        let mut buffer = Vec::new();
        let result = result(Vec::new(), true);
        render(&result, &mut buffer).expect("writing to a Vec cannot fail");
        let parsed: Value = serde_json::from_slice(&buffer).expect("the output parses");
        assert_eq!(parsed["dry_run"], true);
    }
}
