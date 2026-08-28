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

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use humansize::{DECIMAL, format_size};
use owo_colors::{OwoColorize, Style};

use super::{Entry, RunResult};
use crate::classify::Provenance;
use crate::delete::Outcome;

/// Spaces between two columns.
const GUTTER: &str = "  ";

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

/// The widest a path column is padded out to before its explanation.
///
/// Alignment stops paying once the column no longer fits the terminal: past this, padding a
/// short path out to match a long sibling pushes the explanation off the right edge, which
/// costs more than the ragged edge it buys. A path over the cap simply runs on and takes its
/// explanation with it — nothing is elided.
const MAX_PATH_COLUMN: usize = 40;

/// How much to show.
#[derive(Debug, Clone, Copy, Default)]
pub struct HumanOptions {
    /// Show a line per skipped candidate with its reason, instead of just a count.
    pub verbose: bool,
    /// Suppress the per-artifact lines and print only the footer.
    pub summary_only: bool,
}

/// The palette.
///
/// Chosen to survive both a light and a dark terminal. Plain ANSI blue is the one standard
/// colour that is unreadable against a dark background in common themes, so `refused` — the
/// state a reader most needs to notice — is cyan, and the ecosystem it displaced is dim. That
/// also leaves the ecosystem column quiet and the `unanchored` case loud, which is the emphasis
/// ADR 0004 asks for.
mod palette {
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

fn outcome_style(outcome: &Outcome) -> Style {
    match outcome {
        Outcome::Removed => palette::removed(),
        Outcome::WouldRemove => palette::would_remove(),
        Outcome::Refused(_) => palette::refused(),
        Outcome::Failed(_) => palette::failed(),
    }
}

/// One rendered line, with every column held as plain text.
///
/// Rows are built before anything is written so a block's column widths can be measured. The
/// alternative — one fixed width per column for the whole report — is what left the skip lines
/// with an empty size and ecosystem, which reads as a hole in the table rather than as a
/// different kind of row.
struct Line {
    label: &'static str,
    label_style: Style,
    /// `None` for rows that have no size, which collapses the column for the whole block.
    size: Option<String>,
    /// The ecosystem, or [`UNANCHORED`]. `None` collapses the column for the whole block.
    tag: Option<&'static str>,
    tag_style: Style,
    path: String,
    detail: Option<String>,
}

/// The widest plain text in each column of one block.
#[derive(Debug, Default, Clone, Copy)]
struct Widths {
    label: usize,
    size: usize,
    tag: usize,
    /// Zero when no row in the block has a detail, so the last column is never padded and the
    /// output never carries trailing whitespace.
    path: usize,
}

impl Widths {
    fn measure(lines: &[Line]) -> Self {
        let mut widths = Self::default();
        for line in lines {
            widths.label = widths.label.max(width_of(line.label));
            if let Some(size) = &line.size {
                widths.size = widths.size.max(width_of(size));
            }
            if let Some(tag) = line.tag {
                widths.tag = widths.tag.max(width_of(tag));
            }
            // Only the rows that carry an explanation are measured: they are the only ones the
            // column exists to line up. A block of removals whose longest path is 106 characters
            // would otherwise pad the one refusal in it out to 106 and push its reason off the
            // right edge, which is the opposite of what alignment is for.
            if line.detail.is_some() {
                widths.path = widths.path.max(width_of(&line.path));
            }
        }
        widths.path = widths.path.min(MAX_PATH_COLUMN);
        widths
    }
}

/// The column count `text` occupies.
///
/// Characters rather than bytes, which is right for the accented and non-Latin path components
/// a filesystem tool meets. It is still not the terminal's idea of width for wide CJK glyphs;
/// getting that right needs a dependency, and the cost of being wrong is a ragged column rather
/// than a wrong answer.
fn width_of(text: &str) -> usize {
    text.chars().count()
}

fn write_fill(out: &mut impl io::Write, spaces: usize) -> io::Result<()> {
    write!(out, "{:spaces$}", "")
}

/// Writes `text` in `style`, then pads it out to `width` columns.
///
/// The fill is written *outside* the styled span, deliberately. `owo-colors` forwards the
/// formatter to the value it wraps, so `{:<12}` over a styled `&str` does align on the plain
/// text — but it emits the blanks between the escapes, painting them, and it silently stops
/// padding at all for any wrapped type whose `Display` does not call `Formatter::pad`.
/// Measuring here removes both hazards.
fn write_column(out: &mut impl io::Write, text: &str, style: Style, width: usize) -> io::Result<()> {
    write!(out, "{}", style.style(text))?;
    write_fill(out, width.saturating_sub(width_of(text)))
}

fn write_column_right(out: &mut impl io::Write, text: &str, style: Style, width: usize) -> io::Result<()> {
    write_fill(out, width.saturating_sub(width_of(text)))?;
    write!(out, "{}", style.style(text))
}

fn write_line(line: &Line, widths: Widths, out: &mut impl io::Write) -> io::Result<()> {
    write!(out, "{GUTTER}")?;
    write_column(out, line.label, line.label_style, widths.label)?;

    if widths.size > 0 {
        write!(out, "{GUTTER}")?;
        write_column_right(
            out,
            line.size.as_deref().unwrap_or_default(),
            palette::quiet(),
            widths.size,
        )?;
    }
    if widths.tag > 0 {
        write!(out, "{GUTTER}")?;
        write_column(out, line.tag.unwrap_or_default(), line.tag_style, widths.tag)?;
    }

    write!(out, "{GUTTER}{}", line.path)?;
    match &line.detail {
        None => writeln!(out),
        Some(detail) => {
            write_fill(out, widths.path.saturating_sub(width_of(&line.path)))?;
            writeln!(
                out,
                "{GUTTER}{}",
                format_args!("{BECAUSE}{detail}").style(palette::quiet())
            )
        }
    }
}

fn write_block(lines: &[Line], out: &mut impl io::Write) -> io::Result<()> {
    let widths = Widths::measure(lines);
    for line in lines {
        write_line(line, widths, out)?;
    }
    Ok(())
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

fn artifact_line(entry: &Entry, base: Option<&Path>) -> Line {
    let detail = match &entry.outcome {
        Outcome::Refused(refusal) => Some(refusal.to_string()),
        Outcome::Failed(message) => Some(message.clone()),
        Outcome::Removed | Outcome::WouldRemove => match &entry.finding.provenance {
            // Shortened against the same base as the path. An `include` pattern is usually
            // the path it matched, so printing it in full puts the longest string on the line
            // twice and undoes the width this layout exists for.
            Provenance::Included { pattern } => {
                let shown = shorten(Path::new(pattern), base);
                (shown != shorten(&entry.path, base))
                    .then(|| format!("from config `{shown}`"))
                    .or_else(|| Some("from config".to_owned()))
            }
            Provenance::Anchored { .. } => None,
        },
    };
    let ecosystem = entry.ecosystem();
    Line {
        label: outcome_label(&entry.outcome),
        label_style: outcome_style(&entry.outcome),
        size: Some(format_size(entry.bytes, DECIMAL)),
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
        let artifacts: Vec<Line> = result.entries.iter().map(|entry| artifact_line(entry, base)).collect();
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
    }

    write_footer(result, options, wrote_body, out)
}

/// What was reclaimed, per ecosystem, biggest first.
///
/// A `$HOME` sweep prints a line per artifact and there is no honest way to print fewer: the
/// report is the only record of a destructive act (ADR 0006), so eliding removals is not on the
/// table. What the footer can do is say what the list adds up to, which makes the shape of a
/// 174-line run legible without reading all of it.
///
/// Sorted by bytes because the question this answers is where the space went; the id breaks
/// ties so two runs over an unchanged tree still render identically.
fn reclaimed_by_ecosystem(result: &RunResult) -> Vec<(&'static str, usize, u64)> {
    let mut totals: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();
    for entry in result.entries.iter().filter(|entry| entry.outcome.is_reclaimed()) {
        let slot = totals.entry(entry.ecosystem().unwrap_or(UNANCHORED)).or_default();
        slot.0 += 1;
        slot.1 += entry.bytes;
    }
    let mut rows: Vec<_> = totals
        .into_iter()
        .map(|(id, (count, bytes))| (id, count, bytes))
        .collect();
    rows.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(right.0)));
    rows
}

/// The breakdown line, or `None` when there is only one ecosystem and it would repeat the
/// headline it sits under.
fn ecosystem_summary(result: &RunResult) -> Option<String> {
    let rows = reclaimed_by_ecosystem(result);
    if rows.len() < 2 {
        return None;
    }
    let named: Vec<String> = rows
        .iter()
        .take(MAX_ECOSYSTEMS_NAMED)
        .map(|(id, count, bytes)| format!("{id} {} ({count})", format_size(*bytes, DECIMAL)))
        .collect();
    let line = named.join(", ");
    match rows.len().saturating_sub(MAX_ECOSYSTEMS_NAMED) {
        0 => Some(line),
        hidden => Some(format!("{line}, +{hidden} more")),
    }
}

/// The plural of `noun` for `count`. Every noun the footer uses takes a plain `s`.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_owned()
    } else {
        format!("{noun}s")
    }
}

fn write_footer(
    result: &RunResult,
    options: HumanOptions,
    wrote_body: bool,
    out: &mut impl io::Write,
) -> io::Result<()> {
    let totals = result.totals();
    let verb = if result.dry_run { "reclaimable" } else { "reclaimed" };

    if wrote_body {
        writeln!(out)?;
    }

    writeln!(
        out,
        "{} {}, {} {verb} in {:.2?}",
        totals.reclaimed.bold(),
        plural(totals.reclaimed, "artifact"),
        format_size(totals.bytes, DECIMAL).bold(),
        result.elapsed
    )?;

    if let Some(summary) = ecosystem_summary(result) {
        writeln!(out, "{GUTTER}{}", summary.style(palette::quiet()))?;
    }

    // Totals of different kinds get their own lines. On the one line they used to share, the
    // number that matters — what was reclaimed — was the hardest of the four to find.
    if totals.skipped > 0 {
        let text = format!("{} {} skipped", totals.skipped, plural(totals.skipped, "candidate"));
        write!(out, "{GUTTER}{}", text.style(palette::quiet()))?;
        if options.verbose {
            writeln!(out)?;
        } else {
            writeln!(out, "{GUTTER}{}", "--verbose for why".style(palette::quiet()))?;
        }
    }
    if totals.refused > 0 {
        let text = format!(
            "{} {} refused by a safety rail",
            totals.refused,
            plural(totals.refused, "artifact")
        );
        writeln!(out, "{GUTTER}{}", text.style(palette::refused()))?;
    }
    if totals.failed > 0 {
        let text = format!(
            "{} {} could not be removed",
            totals.failed,
            plural(totals.failed, "artifact")
        );
        writeln!(out, "{GUTTER}{}", text.style(palette::failed()))?;
    }

    if result.dry_run {
        writeln!(out, "{}", "Dry run — nothing was removed.".style(palette::quiet()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
                    Outcome::Failed("Permission denied".to_owned()),
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
            ),
            artifact_line(
                &entry(
                    "/projects/a/target",
                    "rust.target",
                    0,
                    Outcome::Refused(Refusal::Symlink),
                ),
                None,
            ),
        ];

        assert_eq!(Widths::measure(&lines).path, MAX_PATH_COLUMN);
    }

    /// The footer says what a long list adds up to, so a reader does not have to add up 174
    /// lines to learn where the space went.
    #[test]
    fn should_break_the_reclaimed_total_down_by_ecosystem() {
        let text = render_to_string(&mixed(), HumanOptions::default());

        assert!(text.contains("rust 4.20 GB (1), node 18.50 MB (1)"), "{text}");
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
}
