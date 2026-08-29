//! The human renderer.
//!
//! One line per artifact, a summary footer, and — at `--verbose` — a reason for everything
//! voom *didn't* take. The skips matter as much as the removals: marker anchoring deliberately
//! passes over things a naive tool would delete, and without an explanation that reads as the
//! tool randomly missing things rather than as a policy (ADR 0007).
//!
//! Colour is written unconditionally as ANSI and stripped downstream by `anstream`, which is
//! what handles TTY detection, redirection, `NO_COLOR`, `CLICOLOR_FORCE` and Windows consoles.
//! Colour is never the only carrier of meaning — every state is also named in text, every
//! explanation is introduced by [`BECAUSE`], and every column is aligned on its plain text.
//!
//! The report is laid out as up to three blocks — artifacts, skips, walk failures — separated
//! by blank lines, each measured against its own rows. Blocks are measured rather than given
//! fixed widths because line length is the binding constraint: a `$HOME` sweep whose lines wrap
//! costs two terminal rows per artifact and loses the alignment that makes a long list
//! scannable at all. For the same reason paths are printed relative to [`base_path`], which the
//! header states once.

use std::io;
use std::path::{Path, PathBuf};

use humansize::{DECIMAL, format_size};
use owo_colors::{OwoColorize, Style};

mod footer;
mod table;

use table::{Line, write_block};

use super::{Entry, RunResult};
use crate::classify::Provenance;
use crate::delete::FailureKind;
use crate::delete::Outcome;

/// Introduces the explanation that follows a path.
///
/// A separator rather than the parentheses this used to use: a reason may itself contain
/// parentheses — `no rust marker (Cargo.toml) proves it` — and nesting them reads badly. It has
/// to be visible with colour off, because the report is routinely piped to a file.
const BECAUSE: &str = "— ";

/// Printed in the ecosystem column when no marker proved the path.
///
/// ADR 0004 requires an unanchored removal to be visibly labelled as one, so it occupies the
/// column an ecosystem would: that is the column a reader scans to answer "what said this was
/// safe to delete".
const UNANCHORED: &str = "unanchored";

/// How many ecosystems the footer's breakdown names before it starts counting them.
///
/// The breakdown is one line, so it has to fit one; the tail says how many it left out. This
/// caps a *summary*, never the list of what was removed — that list is the only record of a
/// destructive act (ADR 0006) and is never elided.
const MAX_ECOSYSTEMS_NAMED: usize = 6;

/// How much to show.
#[derive(Debug, Clone, Copy, Default)]
pub struct HumanOptions {
    /// Show a line per skipped candidate with its reason, instead of just a count.
    pub verbose: bool,
    /// Suppress the per-artifact lines and print only the footer.
    pub summary_only: bool,
    /// Whether the run carried `--force`, which decides whether suggesting it is useful.
    pub forced: bool,
}

/// The palette.
///
/// Chosen to survive both a light and a dark terminal. Plain ANSI blue is the one standard
/// colour that is unreadable against a dark background in common themes, so `refused` — the
/// state a reader most needs to notice — is cyan, and the ecosystem it displaced is dim. That
/// also leaves the ecosystem column quiet and the `unanchored` case loud, which is the emphasis
/// ADR 0004 asks for.
pub(super) mod palette {
    use owo_colors::Style;

    pub fn removed() -> Style {
        Style::new().green()
    }

    pub fn would_remove() -> Style {
        Style::new().yellow()
    }

    pub fn refused() -> Style {
        Style::new().cyan()
    }

    pub fn failed() -> Style {
        Style::new().red()
    }

    /// Sizes, ecosystems, skips, and every explanation — everything secondary to the outcome.
    pub fn quiet() -> Style {
        Style::new().dimmed()
    }

    pub fn unanchored() -> Style {
        Style::new().magenta()
    }

    /// A removal that freed real space and left the artifact behind.
    ///
    /// Its own function rather than a reuse of `would_remove`, even though the colour matches:
    /// the two can never appear in the same report — a dry run produces no partials — and
    /// sharing one would tie two unrelated decisions together.
    pub fn partial() -> Style {
        Style::new().yellow()
    }
}

/// The word printed for an outcome. Text, not colour, is what carries the meaning.
fn outcome_label(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Removed => "removed",
        Outcome::WouldRemove => "would remove",
        Outcome::Refused(_) => "refused",
        Outcome::PartiallyRemoved { .. } => "partial",
        _ => "failed",
    }
}

fn outcome_style(outcome: &Outcome) -> Style {
    match outcome {
        Outcome::Removed => palette::removed(),
        Outcome::WouldRemove => palette::would_remove(),
        Outcome::Refused(_) => palette::refused(),
        Outcome::PartiallyRemoved { .. } => palette::partial(),
        _ => palette::failed(),
    }
}

/// The directory the report prints paths relative to.
///
/// The single scan root, or the deepest directory containing all of them. A `$HOME` sweep
/// otherwise prints every path from `/Users/…` outward and wraps in a normal terminal; stating
/// the base once in the header and stripping it buys the width back for free.
///
/// `None` when there is nothing worth stripping: a relative root is short already, and
/// stripping `/` would turn absolute paths into relative-looking ones for no saving at all.
fn base_path(roots: &[PathBuf]) -> Option<&Path> {
    let first = roots.first()?;
    let base = first
        .ancestors()
        .find(|candidate| roots.iter().all(|root| root.starts_with(candidate)))?;
    (base.is_absolute() && base.parent().is_some()).then_some(base)
}

/// The path as the report shows it: relative to `base` when it sits below it.
fn shorten(path: &Path, base: Option<&Path>) -> String {
    let Some(base) = base else {
        return path.display().to_string();
    };
    match path.strip_prefix(base) {
        // The scan root itself, which the header has already named.
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Ok(relative) => relative.display().to_string(),
        // A path outside every root — a configured `include` can name one. Absolute is the only
        // honest rendering, and it stands out from its neighbours, which is right.
        Err(_) => path.display().to_string(),
    }
}

/// Appends what would plausibly get the rest of it, when there is anything to suggest.
///
/// Silent when the run already carried `--force`: telling a user who just passed it to pass it
/// is the failure mode the whole "say what happened in words" rewrite was against, and the shape
/// that reaches here under `--force` is the one `--force` deliberately cannot fix — an
/// obstruction in the artifact's *parent*.
fn with_hint(detail: &str, kind: FailureKind, forced: bool) -> String {
    match kind.hint().filter(|_| !forced) {
        Some(hint) => format!("{detail}; {hint}"),
        None => detail.to_owned(),
    }
}

/// What is left of a partly removed artifact, in words rather than only in bytes.
///
/// An emptied directory occupies no blocks on most filesystems, so the honest byte count for one
/// is zero — and `0 B left` beside a path that is still on disk reads as "nothing left". The
/// artifact is what is left, whatever it weighs.
pub(super) fn remaining_words(remaining: u64) -> String {
    if remaining == 0 {
        return "the directory itself is still there".to_owned();
    }
    format!("{} left", format_size(remaining, DECIMAL))
}

fn artifact_line(entry: &Entry, base: Option<&Path>, forced: bool) -> Line {
    let detail = match &entry.outcome {
        Outcome::Refused(refusal) => Some(refusal.to_string()),
        // What is *left* leads, because it is what the reader has to act on. The size column
        // already shows what went, so repeating it here would spend the line on the good news.
        Outcome::PartiallyRemoved { remaining, failure, .. } => Some(with_hint(
            // "still there" rather than a bare size: the residue is measured in bytes and an
            // emptied directory occupies none of them on most filesystems, so `0 B left` read as
            // "nothing left" about an artifact that was still sitting on disk.
            &format!("{}; {failure}", remaining_words(*remaining)),
            failure.kind(),
            forced,
        )),
        Outcome::Failed(failure) => Some(with_hint(&failure.to_string(), failure.kind(), forced)),
        Outcome::Removed | Outcome::WouldRemove => match &entry.finding.provenance {
            // Shortened against the same base as the path. An `include` pattern is usually
            // the path it matched, so printing it in full puts the longest string on the line
            // twice and undoes the width this layout exists for.
            // "included by name" rather than "from config": the same provenance now also
            // comes from `--include`, and telling a reader to go looking in a `voom.toml`
            // that has nothing to do with it is worse than saying less.
            Provenance::Included { pattern } => {
                let shown = shorten(Path::new(pattern), base);
                (shown != shorten(&entry.path, base))
                    .then(|| format!("included by `{shown}`"))
                    .or_else(|| Some("included by name".to_owned()))
            }
            // A cache says nothing here either: the ecosystem column already carries its id,
            // and the note about what removing it costs belongs to `voom caches` rather than
            // to every line of every report.
            Provenance::Anchored { .. } | Provenance::Cache { .. } => None,
        },
    };
    let ecosystem = entry.ecosystem();
    Line {
        label: outcome_label(&entry.outcome),
        label_style: outcome_style(&entry.outcome),
        size: Some(format_size(entry.reclaimed_bytes(), DECIMAL)),
        tag: Some(ecosystem.unwrap_or(UNANCHORED)),
        tag_style: if ecosystem.is_some() {
            palette::quiet()
        } else {
            palette::unanchored()
        },
        path: shorten(&entry.path, base),
        detail,
    }
}

fn skip_lines(result: &RunResult, base: Option<&Path>) -> Vec<Line> {
    result
        .skip_reasons()
        .map(|(path, reason)| Line {
            label: "skipped",
            label_style: palette::quiet(),
            size: None,
            tag: None,
            tag_style: palette::quiet(),
            path: shorten(path, base),
            detail: Some(reason.to_string()),
        })
        .collect()
}

fn failure_lines(result: &RunResult, verbose: bool, base: Option<&Path>) -> Vec<Line> {
    result
        .failures
        .iter()
        // A transient interruption is not something the reader can act on, and a $HOME sweep
        // produces a handful every time. Listing them beside real permission failures is what
        // teaches people to skim the section.
        .filter(|failure| verbose || !failure.transient)
        .map(|failure| Line {
            label: if failure.transient { "interrupted" } else { "walk error" },
            label_style: if failure.transient {
                palette::quiet()
            } else {
                palette::failed()
            },
            size: None,
            tag: None,
            tag_style: palette::quiet(),
            path: failure
                .path
                .as_deref()
                .map_or_else(|| "-".to_owned(), |path| shorten(path, base)),
            detail: Some(failure.message.clone()),
        })
        .collect()
}

/// Renders the whole report.
///
/// # Errors
///
/// Propagates any write failure from `out`.
pub fn render(result: &RunResult, options: HumanOptions, out: &mut impl io::Write) -> io::Result<()> {
    let mut wrote_body = false;

    if !options.summary_only {
        let base = base_path(&result.roots);
        let artifacts: Vec<Line> = result
            .entries
            .iter()
            .map(|entry| artifact_line(entry, base, options.forced))
            .collect();
        let skips = if options.verbose {
            skip_lines(result, base)
        } else {
            Vec::new()
        };
        let failures = failure_lines(result, options.verbose, base);

        if let Some(base) = base.filter(|_| !(artifacts.is_empty() && skips.is_empty() && failures.is_empty())) {
            writeln!(out, "{}", base.display().bold())?;
            wrote_body = true;
        }
        let mut wrote_block = false;
        for block in [&artifacts, &skips, &failures] {
            if block.is_empty() {
                continue;
            }
            // A blank line between blocks. It is what says the skips are a different kind of
            // row rather than a continuation of the artifact table with columns missing.
            if wrote_block {
                writeln!(out)?;
            }
            write_block(block, out)?;
            (wrote_block, wrote_body) = (true, true);
        }

        // Git housekeeping speaks only when it has something to say. It runs in nearly every
        // repository a sweep walks past and nearly always finds nothing to do, so a block that
        // appeared whenever it *ran* would be on almost every report and would train the reader
        // to skim the region where refusals and failures live.
        if let Some(git) = result
            .git
            .as_ref()
            .filter(|git| !git.noteworthy(options.verbose).is_empty())
        {
            if wrote_block {
                writeln!(out)?;
            }
            crate::git::write_sweep_block(git, base, options.verbose, out)?;
            wrote_body = true;
        }
    }

    footer::write_footer(result, options, wrote_body, out)
}
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::footer::{ecosystem_summary, reclaimed_by_ecosystem};
    use super::table::{MAX_PATH_COLUMN, Widths};

    use super::*;
    use crate::classify::SkipReason;
    use crate::delete::Refusal;
    use crate::report::fixtures;
    use crate::report::fixtures::{spell, unspell};
    use crate::scan::Skipped;

    // This renderer is the one that shortens paths against a base, so unlike the JSON tests
    // these fixtures have to be absolute *on the platform the test runs on*. The wrappers below
    // spell the literals accordingly and `render_to_string` maps the output back, so the call
    // sites and the snapshots stay in one Unix-shaped spelling. See `fixtures::spell`.

    fn entry(path: &str, spec: &str, bytes: u64, outcome: Outcome) -> Entry {
        fixtures::entry(&spell(path), spec, bytes, outcome)
    }

    fn included(path: &str, pattern: &str, bytes: u64, outcome: Outcome) -> Entry {
        fixtures::included(&spell(path), &spell(pattern), bytes, outcome)
    }

    fn result(entries: Vec<Entry>, dry_run: bool) -> RunResult {
        RunResult {
            roots: vec![PathBuf::from(spell("/projects"))],
            ..fixtures::result(entries, dry_run)
        }
    }

    fn path(literal: &str) -> PathBuf {
        PathBuf::from(spell(literal))
    }

    /// Renders to a string with ANSI removed, which is what `anstream` does downstream when the
    /// destination is not a terminal. Snapshots stay readable and colour stays exercised.
    fn render_to_string(result: &RunResult, options: HumanOptions) -> String {
        let mut buffer = Vec::new();
        render(result, options, &mut buffer).expect("writing to a Vec cannot fail");
        let text = String::from_utf8(buffer).expect("the renderer emits UTF-8");
        unspell(&anstream::adapter::strip_str(&text).to_string())
    }

    fn verbose() -> HumanOptions {
        HumanOptions {
            verbose: true,
            summary_only: false,
            forced: false,
        }
    }

    /// A run whose walk hit one real failure and one interruption.
    fn interrupted() -> RunResult {
        let mut result = result(Vec::new(), true);
        result.failures = vec![
            crate::scan::WalkFailure {
                path: Some(path("/projects/locked")),
                message: "Permission denied (os error 13)".to_owned(),
                transient: false,
            },
            crate::scan::WalkFailure {
                path: Some(path("/projects/containers")),
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
        let text = render_to_string(&interrupted(), verbose());

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
            vec![included(
                "/scratch/build-junk",
                "/scratch/build-junk",
                1024,
                Outcome::Removed,
            )],
            false,
        );

        let text = render_to_string(&result, HumanOptions::default());

        assert!(text.contains("unanchored"), "{text}");
        assert!(text.contains("included by"), "{text}");
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
                    Outcome::Failed(crate::report::fixtures::failure(std::io::ErrorKind::PermissionDenied)),
                ),
                entry(
                    "/projects/busy/target",
                    "rust.target",
                    130_500_000_000,
                    Outcome::PartiallyRemoved {
                        freed: 130_000_000_000,
                        remaining: 500_000_000,
                        failure: crate::report::fixtures::failure(std::io::ErrorKind::DirectoryNotEmpty),
                    },
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

    fn skipped() -> RunResult {
        let mut result = result(
            vec![entry("/projects/api/target", "rust.target", 100, Outcome::Removed)],
            false,
        );
        result.skipped_count = 2;
        result.skips = vec![
            Skipped {
                path: path("/projects/data/target"),
                reason: SkipReason::NoMarker {
                    ecosystem: "rust",
                    markers: &["Cargo.toml"],
                },
            },
            Skipped {
                path: path("/projects/tool/bin"),
                reason: SkipReason::NotEnabled {
                    spec: "go.bin".to_owned(),
                    note: Some("`bin/` beside a go.mod is as often hand-written scripts as output."),
                },
            },
        ];
        result.sort();
        result
    }

    #[test]
    fn should_show_a_skip_count_by_default_and_reasons_when_verbose() {
        insta::assert_snapshot!(
            "default_hides_skip_detail",
            render_to_string(&skipped(), HumanOptions::default())
        );
        insta::assert_snapshot!("verbose_explains_every_skip", render_to_string(&skipped(), verbose()));
    }

    /// The skip lines are their own block with their own widths. Sharing the artifact table's
    /// columns left them with an empty size and ecosystem, which reads as a hole in the table.
    #[test]
    fn should_not_leave_empty_columns_on_a_skip_line() {
        let text = render_to_string(&skipped(), verbose());
        let skip = text.lines().find(|line| line.contains("skipped")).expect("a skip line");

        assert!(
            !skip.contains("          "),
            "no run of blanks where a size and an ecosystem would be: {skip:?}"
        );
    }

    /// A reason may itself contain parentheses, so wrapping it in another pair reads badly.
    #[test]
    fn should_introduce_a_reason_without_nesting_parentheses() {
        let text = render_to_string(&skipped(), verbose());

        assert!(text.contains("— no rust marker (Cargo.toml) proves it"), "{text}");
        assert!(!text.contains("(no rust marker"), "{text}");
    }

    #[test]
    fn should_render_only_the_footer_when_asked_for_a_summary() {
        let options = HumanOptions {
            verbose: false,
            summary_only: true,
            forced: false,
        };
        insta::assert_snapshot!(render_to_string(&mixed(), options));
    }

    #[test]
    fn should_render_a_clean_tree_without_a_leading_blank_line() {
        insta::assert_snapshot!(render_to_string(&result(Vec::new(), false), HumanOptions::default()));
    }

    /// The motivating case: a `$HOME` sweep whose absolute paths wrap in a normal terminal.
    #[test]
    fn should_print_paths_relative_to_the_scan_root_named_in_the_header() {
        let text = render_to_string(&mixed(), HumanOptions::default());

        assert!(text.starts_with("/projects\n"), "the root is stated once: {text}");
        assert!(text.contains("  api/target\n"), "{text}");
        assert!(
            !text.contains(" /projects/api/target"),
            "no line repeats the root: {text}"
        );
    }

    /// A path a configured `include` named can sit outside every scan root. Rendering it
    /// relative to a base it is not under would be a lie about where it is.
    #[test]
    fn should_print_a_path_outside_the_roots_in_full() {
        let result = result(
            vec![included("/scratch/build-junk", "/scratch/*", 1024, Outcome::Removed)],
            false,
        );

        let text = render_to_string(&result, HumanOptions::default());

        assert!(text.contains("/scratch/build-junk"), "{text}");
    }

    /// With several roots the header names the directory they share, so one base still serves
    /// every line and no path is rendered relative to a root it does not belong to.
    #[test]
    fn should_use_the_common_parent_as_the_base_for_several_roots() {
        let roots = vec![path("/projects/api"), path("/projects/web")];
        assert_eq!(base_path(&roots), Some(path("/projects").as_path()));
    }

    /// Stripping `/` would make absolute paths look relative and save nothing; a relative root
    /// is short already.
    #[test]
    fn should_not_shorten_against_a_useless_base() {
        assert_eq!(base_path(&[path("/a"), path("/b")]), None);
        assert_eq!(base_path(&[PathBuf::from(".")]), None);
        assert_eq!(base_path(&[]), None);
    }

    /// A refusal buried in a block of removals with very long paths must still have its reason
    /// on screen: the column lines up the rows that have one, not the whole block.
    #[test]
    fn should_not_pad_a_reason_out_to_the_longest_path_in_the_block() {
        let long = format!("/projects/{}/target", "deep/".repeat(20));
        let result = result(
            vec![
                entry(&long, "rust.target", 1_000, Outcome::Removed),
                entry(
                    "/projects/link/target",
                    "rust.target",
                    0,
                    Outcome::Refused(Refusal::Symlink),
                ),
            ],
            false,
        );

        let text = render_to_string(&result, HumanOptions::default());

        assert!(text.contains("link/target  — is a symlink"), "{text}");
    }

    /// Even among rows that have one, the column is bounded, so one very long path cannot push
    /// every other explanation off the right edge.
    #[test]
    fn should_bound_the_path_column() {
        let long = format!("/projects/{}/target", "deep/".repeat(20));
        let lines = vec![
            artifact_line(
                &entry(&long, "rust.target", 0, Outcome::Refused(Refusal::Symlink)),
                None,
                false,
            ),
            artifact_line(
                &entry(
                    "/projects/a/target",
                    "rust.target",
                    0,
                    Outcome::Refused(Refusal::Symlink),
                ),
                None,
                false,
            ),
        ];

        assert_eq!(Widths::measure(&lines).path, MAX_PATH_COLUMN);
    }

    /// The footer says what a long list adds up to, so a reader does not have to add up 174
    /// lines to learn where the space went.
    ///
    /// Rust's total carries the partial removal's freed bytes while its *count* stays at one,
    /// because only one rust artifact is actually gone — the same split the headline makes.
    #[test]
    fn should_break_the_reclaimed_total_down_by_ecosystem() {
        let text = render_to_string(&mixed(), HumanOptions::default());

        assert!(text.contains("rust 134.20 GB (1), node 18.50 MB (1)"), "{text}");
    }

    /// The breakdown and the headline are two accumulators over the same entries, and before
    /// they shared [`Entry::reclaimed_bytes`] they each decided independently what counted — so
    /// a partial removal was exactly the case that would have made them disagree.
    #[test]
    fn the_ecosystem_breakdown_sums_to_the_footer_total() {
        for result in [mixed(), skipped(), interrupted()] {
            let breakdown: u64 = reclaimed_by_ecosystem(&result).iter().map(|row| row.2).sum();

            assert_eq!(
                breakdown,
                result.totals().bytes,
                "the per-ecosystem rows must add up to the headline"
            );
        }
    }

    /// One ecosystem makes the breakdown a restatement of the headline above it.
    #[test]
    fn should_omit_the_breakdown_when_there_is_only_one_ecosystem() {
        assert_eq!(ecosystem_summary(&skipped()), None);
    }

    /// The breakdown is a summary and is capped to one line — but it says how many it left out,
    /// because a total that quietly omits part of itself is worse than no total.
    #[test]
    fn should_say_how_many_ecosystems_the_breakdown_left_out() {
        let specs = [
            "rust.target",
            "node.dist",
            "go.bin",
            "python.build",
            "maven.target",
            "dotnet.obj",
            "zig.zig-out",
        ];
        let entries = specs
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                let bytes = (specs.len() - index) as u64 * 1_000_000;
                entry(&format!("/projects/p{index}/artifact"), spec, bytes, Outcome::Removed)
            })
            .collect();

        let summary = ecosystem_summary(&result(entries, false)).expect("several ecosystems");

        assert_eq!(summary.matches('(').count(), MAX_ECOSYSTEMS_NAMED);
        assert!(summary.ends_with(", +1 more"), "{summary}");
    }

    /// Trailing blanks are invisible to a reader and noisy in a diff of two saved reports.
    #[test]
    fn should_never_write_trailing_whitespace() {
        for options in [HumanOptions::default(), verbose()] {
            for result in [&mixed(), &skipped(), &interrupted()] {
                for line in render_to_string(result, options).lines() {
                    assert_eq!(line, line.trim_end(), "trailing blanks on {line:?}");
                }
            }
        }
    }

    /// Column fill is written outside the styled spans, so no blank is ever painted and a
    /// column cannot come to depend on a wrapped type's `Display` honouring a width.
    #[test]
    fn should_write_column_fill_outside_the_styled_span() {
        let mut buffer = Vec::new();
        render(&mixed(), HumanOptions::default(), &mut buffer).expect("writing to a Vec cannot fail");
        let coloured = String::from_utf8(buffer).expect("the renderer emits UTF-8");

        assert!(coloured.contains('\u{1b}'), "the renderer emits colour");
        // Left-aligned fill follows a reset and right-aligned fill precedes an opening code, so
        // the only thing that can go wrong is a blank on the *inside* of either boundary.
        for opening in ["\u{1b}[1m", "\u{1b}[2m", "\u{1b}[31m", "\u{1b}[32m", "\u{1b}[33m"] {
            assert!(
                !coloured.contains(&format!("{opening} ")),
                "a styled span opens on padding: {coloured:?}"
            );
        }
        for closing in ["\u{1b}[0m", "\u{1b}[39m"] {
            assert!(
                !coloured.contains(&format!(" {closing}")),
                "a styled span closes on padding: {coloured:?}"
            );
        }
        assert_eq!(
            unspell(&anstream::adapter::strip_str(&coloured).to_string()),
            render_to_string(&mixed(), HumanOptions::default()),
            "stripping the colour is exactly what the snapshots pin"
        );
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

    /// ADR 0001 promised dependency directories would never be removed, and its amendment made
    /// them reachable only by an explicit opt-in. A run that took one has to say so in words —
    /// a reader who sees `node_modules` in a list of artifacts should not have to know the
    /// catalog to realise what just happened.
    #[test]
    fn should_name_a_dependency_directory_it_removed() {
        let result = fixtures::result(
            vec![
                fixtures::entry(&spell("/projects/api/target"), "rust.target", 4_200, Outcome::Removed),
                fixtures::entry(
                    &spell("/projects/web/node_modules"),
                    "node.node_modules",
                    9_100,
                    Outcome::Removed,
                ),
            ],
            false,
        );

        let rendered = render_to_string(&result, HumanOptions::default());

        assert!(
            rendered.contains("1 of them is a dependency directory — a re-download, not build output"),
            "the footer must name it:\n{rendered}"
        );
        assert_eq!(result.totals().dependencies, 1);
        assert_eq!(
            result.totals().reclaimed,
            2,
            "and it still counts as an artifact removed"
        );
    }

    /// Two artifacts, neither of them a dependency: the line must not appear at all. A footer
    /// that always carries it would train the reader to skim the one place a warning belongs.
    #[test]
    fn should_say_nothing_about_dependencies_when_none_were_removed() {
        let result = fixtures::result(
            vec![fixtures::entry(
                &spell("/projects/api/target"),
                "rust.target",
                4_200,
                Outcome::Removed,
            )],
            false,
        );

        let rendered = render_to_string(&result, HumanOptions::default());

        assert!(!rendered.contains("dependency"), "no dependency line:\n{rendered}");
    }

    /// A partly removed artifact says the directory is still there, not that nothing is left.
    ///
    /// The residue is measured in bytes, and an emptied directory occupies none of them on most
    /// filesystems — so the honest byte count is zero, and `0 B left` beside a path that is
    /// still on disk reads as "nothing left". The artifact is what is left, whatever it weighs.
    #[test]
    fn should_not_report_an_emptied_directory_as_nothing_left() {
        let entry = fixtures::entry(
            &spell("/projects/api/target"),
            "rust.target",
            4_200,
            Outcome::PartiallyRemoved {
                freed: 4_200,
                remaining: 0,
                failure: fixtures::failure(std::io::ErrorKind::PermissionDenied),
            },
        );
        let rendered = render_to_string(&fixtures::result(vec![entry], false), HumanOptions::default());

        assert!(
            rendered.contains("the directory itself is still there"),
            "an emptied directory is still an artifact on disk:\n{rendered}"
        );
        assert!(
            !rendered.contains("0 B left"),
            "and never reads as nothing left:\n{rendered}"
        );
    }

    /// The `--force` hint is silent on a run that already carried `--force`.
    ///
    /// The one shape that reaches here under it is the shape it deliberately cannot fix — an
    /// obstruction in the artifact's parent — so suggesting the flag the user just passed is
    /// exactly the unhelpful prose the failure messages were rewritten to remove.
    #[test]
    fn should_not_suggest_force_to_a_run_that_already_forced() {
        let entry = fixtures::entry(
            &spell("/projects/api/target"),
            "rust.target",
            4_200,
            Outcome::Failed(fixtures::failure(std::io::ErrorKind::PermissionDenied)),
        );
        let result = fixtures::result(vec![entry], false);

        let plain = render_to_string(&result, HumanOptions::default());
        let forced = render_to_string(
            &result,
            HumanOptions {
                forced: true,
                ..HumanOptions::default()
            },
        );

        assert!(plain.contains("--force"), "an unforced run is told about it:\n{plain}");
        assert!(!forced.contains("--force"), "a forced run is not:\n{forced}");
    }
}
