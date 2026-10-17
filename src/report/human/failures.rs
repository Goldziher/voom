//! The walk-error block: what the walker could not read, kept short enough to leave the
//! artifacts visible.
//!
//! Split out of the renderer when it crossed the module cap. Interruptions and
//! permission-protected directories are not listed by default — the first cannot be acted on and
//! the second is counted in the footer — so what remains here is the failures a reader can fix.

use std::path::Path;

use super::table::Line;
use super::{palette, shorten};
use crate::report::RunResult;

/// How many walk errors with one and the same message are listed before the rest collapse into a
/// count. A `$HOME` sweep on macOS meets a hundred-odd privacy-protected directories under
/// `~/Library`, every one `Operation not permitted`; listing them all buries the artifacts.
pub(super) const COLLAPSE_AFTER: usize = 5;

pub(super) fn failure_lines(result: &RunResult, verbose: bool, base: Option<&Path>) -> Vec<Line> {
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut hidden: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut lines: Vec<Line> = result
        .failures
        .iter()
        // A transient interruption is not something the reader can act on, and a $HOME sweep
        // produces a handful every time. Listing them beside real permission failures is what
        // teaches people to skim the section.
        .filter(|failure| verbose || !failure.transient)
        // Counted in the footer instead; see `footer::write_footer`.
        .filter(|failure| verbose || !failure.inaccessible)
        .filter(|failure| {
            if verbose {
                return true;
            }
            let count = seen.entry(failure.message.as_str()).or_default();
            *count += 1;
            if *count > COLLAPSE_AFTER {
                *hidden.entry(failure.message.as_str()).or_default() += 1;
                false
            } else {
                true
            }
        })
        .map(|failure| Line {
            label: match (failure.transient, failure.inaccessible) {
                (true, _) => "interrupted",
                (_, true) => "inaccessible",
                _ => "walk error",
            },
            label_style: if failure.transient || failure.inaccessible {
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
        .collect();
    lines.extend(hidden.into_iter().map(|(message, count)| Line {
        label: "walk error",
        label_style: palette::failed(),
        size: None,
        tag: None,
        tag_style: palette::quiet(),
        path: format!("… and {count} more"),
        detail: Some(format!("{message} (--verbose lists them all)")),
    }));
    lines
}
