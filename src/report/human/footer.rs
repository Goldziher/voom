//! The footer: what the run reclaimed, where the time went, and what it could not say.
//!
//! Split out of the renderer when it crossed the module cap, along the seam the human report
//! already has — the artifact table above and the summary below it answer different questions and
//! share nothing but the gutter.

use std::collections::BTreeMap;
use std::io;

use humansize::{DECIMAL, format_size};
use owo_colors::OwoColorize;

use super::table::GUTTER;
use super::{HumanOptions, MAX_ECOSYSTEMS_NAMED, UNANCHORED, palette};
use crate::delete::Outcome;
use crate::report::RunResult;

/// What was reclaimed, per ecosystem, biggest first.
///
/// A `$HOME` sweep prints a line per artifact and there is no honest way to print fewer: the
/// report is the only record of a destructive act (ADR 0006), so eliding removals is not on the
/// table. What the footer can do is say what the list adds up to, which makes the shape of a
/// 174-line run legible without reading all of it.
///
/// Sorted by bytes because the question this answers is where the space went; the id breaks
/// ties so two runs over an unchanged tree still render identically.
pub(super) fn reclaimed_by_ecosystem(result: &RunResult) -> Vec<(&'static str, usize, u64)> {
    let mut totals: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();
    for entry in &result.entries {
        let bytes = entry.reclaimed_bytes();
        if bytes == 0 && !entry.outcome.is_reclaimed() {
            continue;
        }
        let slot = totals.entry(entry.ecosystem().unwrap_or(UNANCHORED)).or_default();
        // Counted only when the artifact is actually gone, and totalled whether or not — which
        // is exactly how the headline splits `reclaimed` from `bytes`. Reading the same accessor
        // is what stops the two from drifting apart again.
        if entry.outcome.is_reclaimed() {
            slot.0 += 1;
        }
        slot.1 += bytes;
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
pub(super) fn ecosystem_summary(result: &RunResult) -> Option<String> {
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
/// Bytes left behind by every partial removal in the run.
fn remaining_bytes(result: &RunResult) -> u64 {
    result
        .entries
        .iter()
        .filter_map(|entry| match &entry.outcome {
            Outcome::PartiallyRemoved { remaining, .. } => Some(*remaining),
            _ => None,
        })
        .sum()
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_owned()
    } else {
        format!("{noun}s")
    }
}

pub(super) fn write_footer(
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
        result.timings.total
    )?;

    writeln!(out, "{GUTTER}{}", stage_breakdown(result).style(palette::quiet()))?;

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

    if totals.unmeasured > 0 {
        // The one number in this footer that is knowingly wrong, said out loud. Everything else
        // here is exact; presenting a lower bound in the same typeface without a word would make
        // the whole footer less trustworthy, not more.
        let text = format!(
            "{} {} could not be measured in full — a directory inside could not be read, so the total is a lower bound",
            totals.unmeasured,
            plural(totals.unmeasured, "artifact"),
        );
        writeln!(out, "{GUTTER}{}", text.style(palette::failed()))?;
    }

    if totals.dependencies > 0 {
        // Named outright rather than left inside the headline: ADR 0001 promised these would
        // never be removed, and its amendment made them reachable only by explicit opt-in. A
        // run that took one owes the reader the sentence.
        let noun = if totals.dependencies == 1 {
            "directory"
        } else {
            "directories"
        };
        let text = format!(
            "{} of them {} a dependency {noun} — a re-download, not build output",
            totals.dependencies,
            if totals.dependencies == 1 { "is" } else { "are" },
        );
        writeln!(out, "{GUTTER}{}", text.style(palette::refused()))?;
    }

    if totals.partial > 0 {
        // "counted above" is the whole point of the line: without it a reader who sees an
        // artifact still on disk has no way to know whether its bytes made it into the total.
        let text = format!(
            "{} {} partly removed: {} freed and counted above, {}",
            totals.partial,
            plural(totals.partial, "artifact"),
            format_size(totals.partial_bytes, DECIMAL),
            super::remaining_words(remaining_bytes(result)),
        );
        writeln!(out, "{GUTTER}{}", text.style(palette::partial()))?;
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

/// Where the run's time actually went, stage by stage.
///
/// One number cannot be acted on: a sweep that spends nine seconds walking wants a different
/// fix from one that spends nine seconds sizing, and voom's headline claim is about how long it
/// takes. The stages are ADR 0001's pipeline, measured where each is called.
///
/// They deliberately do not sum to the headline. The residue — loading configuration,
/// normalizing roots, building the worker pool — is real but is not something a reader can do
/// anything about, and a footer line nobody can act on is noise.
fn stage_breakdown(result: &RunResult) -> String {
    let timings = &result.timings;
    format!(
        "scan {:.2?} · policy {:.2?} · size {:.2?} · {} {:.2?}{}",
        timings.scan,
        timings.policy,
        timings.size,
        // Under `--dry-run` the stage still runs — it is the same pipeline with the final step
        // withheld — so the number is real and near zero rather than absent. Naming it
        // "withheld" stops a reader reading that near-zero as a fast deletion.
        if result.dry_run { "withheld" } else { "delete" },
        timings.delete,
        // Only when it ran. A stage that was switched off is not a stage that took no time, and
        // printing `git 0ns` on every run of a machine without git says nothing.
        if timings.git.is_zero() {
            String::new()
        } else {
            format!(" · git {:.2?}", timings.git)
        },
    )
}
