//! The human renderer.
//!
//! One line per artifact, a summary footer, and — at `--verbose` — a reason for everything
//! voom *didn't* take. The skips matter as much as the removals: marker anchoring deliberately
//! passes over things a naive tool would delete, and without an explanation that reads as the
//! tool randomly missing things rather than as a policy (ADR 0007).
//!
//! Colour is written unconditionally as ANSI and stripped downstream by `anstream`, which is
//! what handles TTY detection, redirection, `NO_COLOR`, `CLICOLOR_FORCE` and Windows consoles.
//! Colour is never the only carrier of meaning — every state is also named in text.

use std::io;

use humansize::{DECIMAL, format_size};
use owo_colors::OwoColorize;

use super::{Entry, RunResult};
use crate::classify::Provenance;
use crate::delete::Outcome;

/// How much to show.
#[derive(Debug, Clone, Copy, Default)]
pub struct HumanOptions {
    /// Show a line per skipped candidate with its reason, instead of just a count.
    pub verbose: bool,
    /// Suppress the per-artifact lines and print only the footer.
    pub summary_only: bool,
}

/// The word printed for an outcome. Text, not colour, is what carries the meaning.
fn outcome_label(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Removed => "removed",
        Outcome::WouldRemove => "would remove",
        Outcome::Refused(_) => "refused",
        Outcome::Failed(_) => "failed",
    }
}

fn write_entry(entry: &Entry, out: &mut impl io::Write) -> io::Result<()> {
    let label = outcome_label(&entry.outcome);
    let size = format_size(entry.bytes, DECIMAL);
    let path = entry.path.display();

    match &entry.outcome {
        Outcome::Removed => write!(out, "  {:<12}", label.green())?,
        Outcome::WouldRemove => write!(out, "  {:<12}", label.yellow())?,
        Outcome::Refused(_) => write!(out, "  {:<12}", label.blue())?,
        Outcome::Failed(_) => write!(out, "  {:<12}", label.red())?,
    }
    write!(out, " {:>10}", size.dimmed())?;
    match entry.ecosystem() {
        Some(ecosystem) => write!(out, "  {:<10}", ecosystem.cyan())?,
        // ADR 0004 requires an unanchored removal to be visibly labelled as one. It occupies
        // the column an ecosystem would, because that is the column a reader scans to answer
        // "what said this was safe to delete".
        None => write!(out, "  {:<10}", "unanchored".magenta())?,
    }
    write!(out, " {path}")?;

    match &entry.outcome {
        Outcome::Refused(refusal) => writeln!(out, "  ({refusal})"),
        Outcome::Failed(message) => writeln!(out, "  ({message})"),
        Outcome::Removed | Outcome::WouldRemove => match &entry.finding.provenance {
            Provenance::Included { pattern } => writeln!(out, "  (from config `{pattern}`)"),
            Provenance::Anchored { .. } => writeln!(out),
        },
    }
}

/// Renders the whole report.
///
/// # Errors
///
/// Propagates any write failure from `out`.
pub fn render(result: &RunResult, options: HumanOptions, out: &mut impl io::Write) -> io::Result<()> {
    if !options.summary_only {
        for entry in &result.entries {
            write_entry(entry, out)?;
        }

        if options.verbose {
            for (path, reason) in result.skip_reasons() {
                writeln!(
                    out,
                    "  {:<12} {:>10}  {:<10} {}  ({reason})",
                    "skipped".dimmed(),
                    "",
                    "",
                    path.display()
                )?;
            }
        }

        for failure in &result.failures {
            // A transient interruption is not something the reader can act on, and a $HOME
            // sweep produces a handful every time. Listing them here beside real permission
            // failures is what teaches people to skim the section.
            if failure.transient && !options.verbose {
                continue;
            }
            let path = failure
                .path
                .as_ref()
                .map_or_else(|| "-".to_owned(), |path| path.display().to_string());
            if failure.transient {
                writeln!(out, "  {:<12} {path}  ({})", "interrupted".dimmed(), failure.message)?;
            } else {
                writeln!(out, "  {:<12} {path}  ({})", "walk error".red(), failure.message)?;
            }
        }
    }

    write_footer(result, options, out)
}

fn write_footer(result: &RunResult, options: HumanOptions, out: &mut impl io::Write) -> io::Result<()> {
    let totals = result.totals();
    let verb = if result.dry_run { "reclaimable" } else { "reclaimed" };
    let noun = if totals.reclaimed == 1 { "artifact" } else { "artifacts" };

    if !options.summary_only && (!result.entries.is_empty() || totals.skipped > 0) {
        writeln!(out)?;
    }

    write!(
        out,
        "{} {noun}, {} {verb}",
        totals.reclaimed.bold(),
        format_size(totals.bytes, DECIMAL).bold()
    )?;
    if totals.skipped > 0 {
        write!(out, ", {} skipped", totals.skipped)?;
        if !options.verbose {
            write!(out, " ({})", "--verbose for why".dimmed())?;
        }
    }
    if totals.refused > 0 {
        write!(out, ", {} refused", totals.refused)?;
    }
    if totals.failed > 0 {
        write!(out, ", {} {}", totals.failed, "failed".red())?;
    }
    writeln!(out, " in {:.2?}", result.elapsed)?;

    if result.dry_run {
        writeln!(out, "{}", "Dry run — nothing was removed.".dimmed())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::classify::SkipReason;
    use crate::delete::Refusal;
    use crate::report::fixtures::{entry, result};
    use crate::scan::Skipped;

    /// Renders to a string with ANSI removed, which is what `anstream` does downstream when the
    /// destination is not a terminal. Snapshots stay readable and colour stays exercised.
    fn render_to_string(result: &RunResult, options: HumanOptions) -> String {
        let mut buffer = Vec::new();
        render(result, options, &mut buffer).expect("writing to a Vec cannot fail");
        let text = String::from_utf8(buffer).expect("the renderer emits UTF-8");
        anstream::adapter::strip_str(&text).to_string()
    }

    /// A run whose walk hit one real failure and one interruption.
    fn interrupted() -> RunResult {
        let mut result = result(Vec::new(), true);
        result.failures = vec![
            crate::scan::WalkFailure {
                path: Some(PathBuf::from("/projects/locked")),
                message: "Permission denied (os error 13)".to_owned(),
                transient: false,
            },
            crate::scan::WalkFailure {
                path: Some(PathBuf::from("/projects/containers")),
                message: "Interrupted system call (os error 4)".to_owned(),
                transient: true,
            },
        ];
        result
    }

    /// A $HOME sweep produces these every run. Listing them beside the failures a reader can
    /// act on is what teaches people to skim the section those failures appear in.
    #[test]
    fn should_not_list_a_transient_interruption_by_default() {
        let text = render_to_string(&interrupted(), HumanOptions::default());

        assert!(text.contains("Permission denied"), "{text}");
        assert!(!text.contains("Interrupted system call"), "{text}");
    }

    #[test]
    fn should_list_a_transient_interruption_when_asked_for_detail() {
        let text = render_to_string(
            &interrupted(),
            HumanOptions {
                verbose: true,
                ..HumanOptions::default()
            },
        );

        assert!(text.contains("Interrupted system call"), "{text}");
        assert!(
            text.contains("interrupted"),
            "it is labelled as an interruption, not as a walk error: {text}"
        );
    }

    /// ADR 0004 requires an unanchored removal to be labelled as one. Nothing proved this path;
    /// the report has to say so rather than leaving a blank where an ecosystem would be.
    #[test]
    fn should_label_an_unanchored_removal_as_unanchored() {
        let result = result(
            vec![crate::report::fixtures::included(
                "/scratch/build-junk",
                "/scratch/build-junk",
                1024,
                Outcome::Removed,
            )],
            false,
        );

        let text = render_to_string(&result, HumanOptions::default());

        assert!(text.contains("unanchored"), "{text}");
        assert!(text.contains("from config"), "{text}");
    }

    fn mixed() -> RunResult {
        result(
            vec![
                entry("/projects/api/target", "rust.target", 4_200_000_000, Outcome::Removed),
                entry("/projects/web/dist", "node.dist", 18_500_000, Outcome::Removed),
                entry(
                    "/projects/link/target",
                    "rust.target",
                    0,
                    Outcome::Refused(Refusal::Symlink),
                ),
                entry(
                    "/projects/locked/target",
                    "rust.target",
                    900,
                    Outcome::Failed("removing `/projects/locked/target`: Permission denied".to_owned()),
                ),
            ],
            false,
        )
    }

    #[test]
    fn should_render_a_run_with_every_outcome() {
        insta::assert_snapshot!(render_to_string(&mixed(), HumanOptions::default()));
    }

    #[test]
    fn should_render_a_dry_run_as_reclaimable_and_say_nothing_was_removed() {
        let result = result(
            vec![entry(
                "/projects/api/target",
                "rust.target",
                4_200_000_000,
                Outcome::WouldRemove,
            )],
            true,
        );
        insta::assert_snapshot!(render_to_string(&result, HumanOptions::default()));
    }

    #[test]
    fn should_show_a_skip_count_by_default_and_reasons_when_verbose() {
        let mut result = result(
            vec![entry("/projects/api/target", "rust.target", 100, Outcome::Removed)],
            false,
        );
        result.skipped_count = 2;
        result.skips = vec![
            Skipped {
                path: PathBuf::from("/projects/data/target"),
                reason: SkipReason::NoMarker {
                    ecosystem: "rust",
                    markers: &["Cargo.toml"],
                },
            },
            Skipped {
                path: PathBuf::from("/projects/tool/bin"),
                reason: SkipReason::NotEnabled {
                    spec: "go.bin".to_owned(),
                    note: Some("`bin/` beside a go.mod is as often hand-written scripts as output."),
                },
            },
        ];
        result.sort();

        insta::assert_snapshot!(
            "default_hides_skip_detail",
            render_to_string(&result, HumanOptions::default())
        );
        insta::assert_snapshot!(
            "verbose_explains_every_skip",
            render_to_string(
                &result,
                HumanOptions {
                    verbose: true,
                    summary_only: false
                }
            )
        );
    }

    #[test]
    fn should_render_only_the_footer_when_asked_for_a_summary() {
        let options = HumanOptions {
            verbose: false,
            summary_only: true,
        };
        insta::assert_snapshot!(render_to_string(&mixed(), options));
    }

    #[test]
    fn should_render_a_clean_tree_without_a_leading_blank_line() {
        insta::assert_snapshot!(render_to_string(&result(Vec::new(), false), HumanOptions::default()));
    }

    /// Two runs over an unchanged tree must produce byte-identical output, which is what makes
    /// the report diffable and therefore reviewable.
    #[test]
    fn should_be_byte_identical_across_two_renders() {
        let result = mixed();
        assert_eq!(
            render_to_string(&result, HumanOptions::default()),
            render_to_string(&result, HumanOptions::default())
        );
    }
}
