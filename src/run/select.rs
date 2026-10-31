//! The policy stage: which findings are removed, and how big the survivors are.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use rayon::prelude::*;

use super::{Collected, RunOptions, not_enabled, note};
use crate::classify::Finding;
use crate::config::{Resolved, Resolver};
use crate::error::Result;
use crate::size::Measured;

/// Drops the findings that ecosystem selection or an age policy holds back, noting each one.
///
/// What remains is paired with the configuration that governs it, for the size policies that
/// need bytes first.
fn keep_survivors(
    root: &Path,
    resolver: &Resolver,
    findings: Vec<Finding>,
    options: &RunOptions,
    collected: &mut Collected,
) -> Result<Vec<(Finding, Arc<Resolved>)>> {
    let now = SystemTime::now();
    let started = Instant::now();
    let mut survivors: Vec<(Finding, Arc<Resolved>)> = Vec::with_capacity(findings.len());
    for finding in findings {
        let dir = finding.path.parent().unwrap_or(root);
        let config = resolver.for_dir(dir)?;

        // An `include` is the user naming a path outright, so ecosystem selection has nothing
        // to say about it — there is no ecosystem. Keep policies still apply: they are about
        // whether *now* is the moment to remove something, not about what it is.
        if let Some(id) = finding.artifact()
            && !config.is_enabled(&finding.path, id)
        {
            let reason = not_enabled(id);
            note(collected, options.verbose, finding.path, reason);
            continue;
        }

        // `min_age` is answered from the newest modification *anywhere* in the artifact. A
        // build writes deep inside its output directory, and the directory's own mtime does not
        // move when it does — so a single `stat` on the root called a running build weeks old.
        // The walk is paid only when the policy is set.
        let keep = config.keep_for(&finding.path, finding.ecosystem());
        let held = keep
            .min_age
            .and_then(|_| crate::size::newest_mtime(&finding.path))
            .and_then(|modified| keep.holds_by_age(modified, now));
        match held {
            Some(reason) => note(collected, options.verbose, finding.path, reason),
            None => survivors.push((finding, config)),
        }
    }

    collected.policy += started.elapsed();
    Ok(survivors)
}

/// Decides which findings are actually removed, and measures the ones that survive.
///
/// The configuration in effect for an artifact is the one resolved at its own directory, so a
/// repository's committed `voom.toml` governs its own subtree during a `$HOME`-wide run.
///
/// # Errors
///
/// [`Error::Config`] if a `voom.toml` below the root is missing, unreadable, or invalid.
pub(super) fn select(
    root: &Path,
    resolver: &Resolver,
    findings: Vec<Finding>,
    options: &RunOptions,
    collected: &mut Collected,
) -> Result<Selected> {
    let survivors = keep_survivors(root, resolver, findings, options, collected)?;

    // A size policy needs the bytes before deciding. Otherwise removal already counts every
    // unlinked entry, and walking the same cache first just delays reclaiming its space.
    let needs_size = |finding: &Finding, config: &Resolved| {
        let keep = config.keep_for(&finding.path, finding.ecosystem());
        options.dry_run || keep.min_size.is_some() || keep.max_size.is_some()
    };
    let sizes: Vec<Option<Measured>> = if survivors.iter().any(|(finding, config)| needs_size(finding, config)) {
        let started = Instant::now();
        let sizes = survivors
            .par_iter()
            .map(|(finding, config)| needs_size(finding, config).then(|| crate::size::measure_fully(&finding.path)))
            .collect();
        collected.size += started.elapsed();
        sizes
    } else {
        vec![None; survivors.len()]
    };

    // The `min_size` and `max_size` rules are answered from the sizes just measured, so this
    // loop is charged to the policy stage rather than to sizing.
    let started = Instant::now();
    let mut to_remove: Vec<(Finding, Option<Measured>)> = Vec::with_capacity(survivors.len());
    for ((finding, config), measured) in survivors.into_iter().zip(sizes) {
        let keep = config.keep_for(&finding.path, finding.ecosystem());
        // Against the bytes that could be read. An artifact holding an unreadable directory
        // measures small, so a `min_size` can keep something far larger than the threshold —
        // which is why the count travels with the number instead of being discarded here.
        match measured.and_then(|size| keep.holds_by_size(size.bytes)) {
            Some(reason) => note(collected, options.verbose, finding.path, reason),
            None => to_remove.push((finding, measured)),
        }
    }

    let selected = split_covered(to_remove);
    collected.policy += started.elapsed();

    Ok(selected)
}

/// Sets aside each artifact nested inside another artifact that is also being removed.
fn split_covered(mut to_remove: Vec<(Finding, Option<Measured>)>) -> Selected {
    // An artifact nested inside another artifact that is also being removed is redundant: the
    // outer removal takes it, so removing both would count its bytes twice in the report and
    // then refuse the second as `Vanished`. Only the opt-in dependency artifacts can produce
    // the nesting — the walker prunes at every other match — but the check is general, because
    // the invariant it defends is.
    //
    // Sorted, one path of state: `Path`'s ordering is component-wise, so everything under a
    // kept path sorts immediately after it and before anything that is not under it. Comparing
    // each candidate against the last one kept is therefore enough, and keeps this linear
    // rather than quadratic on a sweep with thousands of artifacts.
    to_remove.sort_by(|(left, _), (right, _)| left.path.cmp(&right.path));
    let mut covering: Option<PathBuf> = None;
    let mut covered = Vec::new();
    to_remove.retain(|(finding, measured)| {
        if let Some(outer) = covering.as_ref().filter(|outer| finding.path.starts_with(outer)) {
            covered.push((finding.clone(), *measured, outer.clone()));
            return false;
        }
        covering = Some(finding.path.clone());
        true
    });

    Selected { to_remove, covered }
}

/// What survived the policy stage: what to remove, and what something else is expected to take.
pub(super) struct Selected {
    pub(super) to_remove: Vec<(Finding, Option<Measured>)>,
    /// Each with the artifact expected to cover it. Held back rather than decided here, because
    /// "already inside X" is only true once X has actually been removed.
    pub(super) covered: Vec<(Finding, Option<Measured>, PathBuf)>,
}
