//! Column measuring and row writing for the human report.
//!
//! Split from the renderer because they answer different questions: this file knows how wide a
//! column is and where the padding goes, and knows nothing about artifacts. Blocks are measured
//! against their own rows rather than given fixed widths, because line length is the binding
//! constraint on a `$HOME` sweep — lines that wrap cost two terminal rows each and lose the
//! alignment that makes a long list scannable at all (ADR 0007).

use std::io;

use owo_colors::{OwoColorize, Style};

use super::{BECAUSE, palette};

/// Spaces between two columns.
pub(super) const GUTTER: &str = "  ";

/// The widest a path column is padded out to before its explanation.
///
/// Alignment stops paying once the column no longer fits the terminal: past this, padding a
/// short path out to match a long sibling pushes the explanation off the right edge, which
/// costs more than the ragged edge it buys. A path over the cap simply runs on and takes its
/// explanation with it — nothing is elided.
pub(super) const MAX_PATH_COLUMN: usize = 40;

/// One rendered line, with every column held as plain text.
///
/// Rows are built before anything is written so a block's column widths can be measured. The
/// alternative — one fixed width per column for the whole report — is what left the skip lines
/// with an empty size and ecosystem, which reads as a hole in the table rather than as a
/// different kind of row.
pub(super) struct Line {
    pub(super) label: &'static str,
    pub(super) label_style: Style,
    /// `None` for rows that have no size, which collapses the column for the whole block.
    pub(super) size: Option<String>,
    /// The ecosystem, or [`UNANCHORED`]. `None` collapses the column for the whole block.
    pub(super) tag: Option<&'static str>,
    pub(super) tag_style: Style,
    pub(super) path: String,
    pub(super) detail: Option<String>,
}

/// The widest plain text in each column of one block.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Widths {
    pub(super) label: usize,
    pub(super) size: usize,
    pub(super) tag: usize,
    /// Zero when no row in the block has a detail, so the last column is never padded and the
    /// output never carries trailing whitespace.
    pub(super) path: usize,
}

impl Widths {
    pub(super) fn measure(lines: &[Line]) -> Self {
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
pub(super) fn width_of(text: &str) -> usize {
    text.chars().count()
}

pub(super) fn write_fill(out: &mut impl io::Write, spaces: usize) -> io::Result<()> {
    write!(out, "{:spaces$}", "")
}

/// Writes `text` in `style`, then pads it out to `width` columns.
///
/// The fill is written *outside* the styled span, deliberately. `owo-colors` forwards the
/// formatter to the value it wraps, so `{:<12}` over a styled `&str` does align on the plain
/// text — but it emits the blanks between the escapes, painting them, and it silently stops
/// padding at all for any wrapped type whose `Display` does not call `Formatter::pad`.
/// Measuring here removes both hazards.
pub(super) fn write_column(out: &mut impl io::Write, text: &str, style: Style, width: usize) -> io::Result<()> {
    write!(out, "{}", style.style(text))?;
    write_fill(out, width.saturating_sub(width_of(text)))
}

pub(super) fn write_column_right(out: &mut impl io::Write, text: &str, style: Style, width: usize) -> io::Result<()> {
    write_fill(out, width.saturating_sub(width_of(text)))?;
    write!(out, "{}", style.style(text))
}

pub(super) fn write_line(line: &Line, widths: Widths, out: &mut impl io::Write) -> io::Result<()> {
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

pub(super) fn write_block(lines: &[Line], out: &mut impl io::Write) -> io::Result<()> {
    let widths = Widths::measure(lines);
    for line in lines {
        write_line(line, widths, out)?;
    }
    Ok(())
}
