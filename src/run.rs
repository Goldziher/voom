//! The pipeline: `scan -> classify -> policy -> size -> delete -> report`.
//!
//! Each stage is a function of the previous one's output. `--dry-run` is this same pipeline
//! with the deleter's final step withheld (ADR 0006) — there is no second code path, so a dry
//! run cannot disagree with a real one.
//!
//! Roots are swept one at a time because configuration, containment and the marker-climb bound
//! are all evaluated relative to the root an entry came from. Within a root everything is
//! parallel.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use indicatif::{ProgressBar, ProgressDrawTarget};
use rayon::prelude::*;

use crate::caches::CacheRoots;
use crate::classify::{ArtifactId, Classifier, Finding, Selection, SkipReason};
use crate::config::resolve::Flags;
use crate::config::{Resolved, Resolver, discover};
use crate::delete::{Guard, Removal};
use crate::error::{Error, Result};
use crate::report::{Entry, RunResult};
use crate::scan::{PatternSet, ScanOptions, Skipped, scan};
use crate::size::measure_all;

/// Everything one invocation needs.
#[expect(
    clippy::struct_excessive_bools,
    reason = "these mirror command-line flags, which are bools by nature"
)]
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Trees to sweep.
    pub roots: Vec<PathBuf>,
    /// Withhold the removal and report what would happen.
    pub dry_run: bool,
    /// Worker threads; `None` means available parallelism.
    pub jobs: Option<usize>,
    /// Stay on the scan root's filesystem.
    pub one_file_system: bool,
    /// Retry a failed removal, repairing permissions inside the artifact first.
    pub force: bool,
    /// Sweep machine-global tool caches and installed toolchains too, which are skipped by
    /// default (ADR 0001).
    pub caches: bool,
    /// Record why candidates were passed over.
    pub verbose: bool,
    /// `--config`, which replaces the discovered hierarchy.
    pub config: Option<PathBuf>,
    /// Command-line overrides, which sit above every configuration layer.
    pub flags: Flags,
    /// Show a scan spinner. Suppressed automatically when stderr is not a terminal, so a
    /// piped or redirected run is never contaminated by it (ADR 0007).
    pub progress: bool,
}

/// Accumulates one root's results into the run's.
#[derive(Default)]
struct Collected {
    entries: Vec<Entry>,
    skips: Vec<Skipped>,
    skipped_count: usize,
    failures: Vec<crate::scan::WalkFailure>,
}

/// Runs the whole pipeline.
///
/// # Errors
///
/// [`Error::ReadDir`] if a scan root cannot be resolved — a root that does not exist is a usage
/// error and must stop the run before anything is removed — [`Error::Config`] for a broken
/// configuration file, [`Error::CatalogPattern`] for an exclusion glob that will not compile, or
/// [`Error::ThreadPool`] if `-j` asks for a pool that cannot be built.
pub fn run(options: &RunOptions) -> Result<RunResult> {
    let started = Instant::now();
    let mut collected = Collected::default();

    let roots = normalize_roots(&options.roots)?;
    let pool = build_pool(options.jobs)?;

    // Sizing and removal fan out over rayon, which otherwise runs on a process-wide pool sized
    // to every logical core. `-j` is the escape hatch for spinning disks and network
    // filesystems (ADR 0005), so it has to bound the removal storm and not just the walk.
    pool.install(|| -> Result<()> {
        for root in &roots {
            sweep(root, options, &mut collected)?;
        }
        Ok(())
    })?;

    let mut result = RunResult {
        roots,
        entries: collected.entries,
        skips: collected.skips,
        skipped_count: collected.skipped_count,
        failures: collected.failures,
        dry_run: options.dry_run,
        elapsed: started.elapsed(),
    };
    result.sort();
    Ok(result)
}

/// Builds the worker pool that sizes and removes artifacts.
///
/// Scoped rather than [`rayon::ThreadPoolBuilder::build_global`], which is process-wide and
/// one-shot: a second call fails, so a global pool would break both the test suite and any
/// program that uses voom as a library and has its own pool.
///
/// # Errors
///
/// [`Error::ThreadPool`] if the pool cannot be built.
fn build_pool(jobs: Option<usize>) -> Result<rayon::ThreadPool> {
    let jobs = jobs.map_or(0, |jobs| jobs.max(1));
    rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|source| Error::ThreadPool { jobs, source })
}

/// Resolves the roots to their real locations, drops duplicates, and drops any root that sits
/// inside another.
///
/// On macOS `/tmp` is a symlink to `/private/tmp`, so `voom /tmp /private/tmp` walks one tree
/// twice and reports every artifact — and every byte — twice over. The same happens for
/// `voom ~ ~/projects`. voom deletes without asking and the report is the only record of what
/// it did, so a doubled total is a correctness bug rather than a cosmetic one.
///
/// # Errors
///
/// [`Error::ReadDir`] if a root cannot be resolved. A root that does not exist is a usage error
/// and must stop the run before anything is removed.
fn normalize_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut resolved: Vec<(PathBuf, usize)> = Vec::with_capacity(roots.len());
    for (index, root) in roots.iter().enumerate() {
        let canonical = root.canonicalize().map_err(|source| Error::ReadDir {
            path: root.clone(),
            source,
        })?;
        resolved.push((canonical, index));
    }

    // Sorting on the canonical path puts an ancestor before everything nested inside it
    // whatever order they were typed in, which is what makes one pass over `kept` sufficient.
    // The index breaks ties, so when two spellings name the same tree the one the user typed
    // first is the one that survives — see the note below on why the choice is visible.
    resolved.sort();

    let mut kept: Vec<(PathBuf, usize)> = Vec::with_capacity(resolved.len());
    for (canonical, index) in resolved {
        if kept.iter().any(|(existing, _)| canonical.starts_with(existing)) {
            continue;
        }
        kept.push((canonical, index));
    }

    // The *original* spelling is what gets walked. Deduplication has to reason about where a
    // root really is, but the walk must not: a user's `exclude = ["/tmp/build/**"]` is written
    // against the path they typed, and silently rewriting it to `/private/tmp/...` would stop
    // the glob matching — an exclusion that quietly stops working is the exact failure ADR 0004
    // exists to prevent.
    //
    // Which is also why the tie-break above is not arbitrary. Exclusion globs are matched
    // textually against the walked path, so of two spellings of one tree only the surviving
    // one's globs keep working. Taking the first the user typed makes that choice predictable
    // instead of alphabetical.
    Ok(kept.into_iter().map(|(_, index)| roots[index].clone()).collect())
}

/// Resolves the configuration that governs one scan root.
///
/// # Errors
///
/// [`Error::Config`] if a configuration file is missing, unreadable, or invalid.
pub fn resolver_for(root: &Path, options: &RunOptions) -> Result<Resolver> {
    let sources = discover(root, options.config.as_deref())?;
    Resolver::new(root, &sources, options.flags.clone())
}

fn sweep(root: &Path, options: &RunOptions, collected: &mut Collected) -> Result<()> {
    let resolver = resolver_for(root, options)?;
    let at_root = resolver.root_config()?;

    // Classification runs permissively: an off-by-default artifact must still be *found* here,
    // because a `voom.toml` deeper in the tree may turn it on. The real decision is made below,
    // against the configuration resolved at each artifact's own location.
    let selection = match &options.flags.ecosystems {
        Some(ids) => {
            let (selection, unknown) = Selection::only(ids.iter().map(String::as_str));
            if let Some(name) = unknown.first() {
                return Err(Error::UnknownEcosystem { name: name.clone() });
            }
            selection.permissive()
        }
        None => Selection::all().permissive(),
    };

    let classifier = Classifier::new(selection)?;
    let roots = std::slice::from_ref(&root.to_path_buf()).to_vec();
    let guard = Guard::new(&roots, options.one_file_system)?;

    let scan_options = ScanOptions {
        jobs: options.jobs,
        one_file_system: options.one_file_system,
        collect_skips: options.verbose,
        exclude: PatternSet::new(at_root.exclude.clone())?,
        include: PatternSet::new(at_root.include.clone())?,
        caches: CacheRoots::for_root(root, options.caches),
    };
    // Progress goes to stderr so it never contaminates piped stdout, and only when stderr is a
    // terminal — a redirected run gets nothing.
    let spinner = start_progress(options.progress, root);
    let scanned = scan(&roots, &classifier, &scan_options);
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    collected.failures.extend(scanned.failures);
    collected.skips.extend(scanned.skips);
    collected.skipped_count += scanned.skipped_count;

    // The configuration in effect for an artifact is the one resolved at its own directory, so
    // a repository's committed `voom.toml` governs its own subtree during a `$HOME`-wide run.
    let now = SystemTime::now();
    let mut survivors: Vec<(Finding, Arc<Resolved>)> = Vec::with_capacity(scanned.findings.len());
    for finding in scanned.findings {
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

        // `min_age` is answered from the artifact's own timestamp, so an artifact held by age
        // costs one `stat` instead of a recursive size walk.
        let keep = config.keep_for(&finding.path, finding.ecosystem());
        let held = std::fs::symlink_metadata(&finding.path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| keep.holds_by_age(modified, now));
        match held {
            Some(reason) => note(collected, options.verbose, finding.path, reason),
            None => survivors.push((finding, config)),
        }
    }

    let paths: Vec<PathBuf> = survivors.iter().map(|(finding, _)| finding.path.clone()).collect();
    let sizes = measure_all(&paths);

    let mut to_remove: Vec<(Finding, u64)> = Vec::with_capacity(survivors.len());
    for ((finding, config), bytes) in survivors.into_iter().zip(sizes) {
        let keep = config.keep_for(&finding.path, finding.ecosystem());
        match keep.holds_by_size(bytes) {
            Some(reason) => note(collected, options.verbose, finding.path, reason),
            None => to_remove.push((finding, bytes)),
        }
    }

    // Removal is independent per artifact and embarrassingly parallel; a failure on one is
    // recorded and never aborts the run.
    let removal = Removal {
        dry_run: options.dry_run,
        force: options.force,
    };
    let entries: Vec<Entry> = to_remove
        .into_par_iter()
        .map(|(finding, bytes)| {
            let outcome = guard.remove(&finding.path, bytes, removal);
            Entry {
                path: finding.path.clone(),
                finding,
                bytes,
                outcome,
            }
        })
        .collect();
    collected.entries.extend(entries);
    Ok(())
}

/// A spinner for the scan phase, or nothing when it would be noise.
fn start_progress(enabled: bool, root: &Path) -> Option<ProgressBar> {
    if !enabled || !std::io::stderr().is_terminal() {
        return None;
    }
    let spinner = ProgressBar::new_spinner();
    spinner.set_draw_target(ProgressDrawTarget::stderr());
    spinner.enable_steady_tick(Duration::from_millis(120));
    spinner.set_message(format!("scanning {}", root.display()));
    Some(spinner)
}

fn not_enabled(artifact: ArtifactId) -> SkipReason {
    SkipReason::NotEnabled {
        spec: artifact.spec(),
        note: artifact.artifact().note,
    }
}

/// Records a skip, keeping only the count unless detail was asked for.
fn note(collected: &mut Collected, verbose: bool, path: PathBuf, reason: SkipReason) {
    collected.skipped_count += 1;
    if verbose {
        collected.skips.push(Skipped { path, reason });
    }
}

/// Helpers shared by this module's test suites.
#[cfg(test)]
mod tests_support {
    use std::path::Path;

    use super::*;

    pub(super) fn options(root: &Path) -> RunOptions {
        RunOptions {
            roots: vec![root.to_path_buf()],
            dry_run: false,
            jobs: Some(2),
            one_file_system: true,
            force: false,
            caches: false,
            verbose: true,
            config: None,
            flags: Flags::default(),
            progress: false,
        }
    }

    pub(super) fn with_keep(root: &Path, keep: crate::policy::KeepPolicy) -> RunOptions {
        RunOptions {
            flags: Flags {
                keep,
                ..Flags::default()
            },
            ..options(root)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use std::time::Duration;

    use super::*;
    use crate::delete::Outcome;
    use crate::policy::KeepPolicy;
    use crate::testing::{snapshot, tree};

    #[test]
    fn should_remove_a_proven_artifact_and_leave_the_source() {
        let fixture = tree(&["Cargo.toml", "src/main.rs", "target/debug/app"]);
        let result = run(&options(fixture.path())).expect("the run completes");

        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].outcome, Outcome::Removed);
        assert_eq!(snapshot(fixture.path()), vec!["Cargo.toml", "src/", "src/main.rs"]);
    }

    /// The whole point, end to end: an unproven `target/` is left exactly where it is.
    #[test]
    fn should_leave_an_unproven_artifact_alone() {
        let fixture = tree(&["target/important.txt"]);
        let before = snapshot(fixture.path());
        let result = run(&options(fixture.path())).expect("the run completes");

        assert!(result.entries.is_empty());
        assert_eq!(snapshot(fixture.path()), before);
        assert_eq!(result.skipped_count, 1);
    }

    #[test]
    fn should_leave_the_tree_byte_identical_during_a_dry_run() {
        let fixture = tree(&["Cargo.toml", "target/debug/app", "package.json", "dist/bundle.js"]);
        let before = snapshot(fixture.path());

        let dry = run(&RunOptions {
            dry_run: true,
            ..options(fixture.path())
        })
        .expect("the dry run completes");
        assert_eq!(snapshot(fixture.path()), before, "a dry run must not touch the tree");
        assert_eq!(dry.entries.len(), 2);
        assert!(dry.entries.iter().all(|entry| entry.outcome == Outcome::WouldRemove));

        let real = run(&options(fixture.path())).expect("the real run completes");
        let dry_paths: Vec<_> = dry.entries.iter().map(|entry| entry.path.clone()).collect();
        let real_paths: Vec<_> = real.entries.iter().map(|entry| entry.path.clone()).collect();
        assert_eq!(
            dry_paths, real_paths,
            "a dry run reports exactly what a real run removes"
        );
    }

    #[test]
    fn should_hold_a_freshly_modified_artifact_when_min_age_is_set() {
        let fixture = tree(&["Cargo.toml", "target/debug/app"]);
        let keep = KeepPolicy {
            min_age: Some(Duration::from_secs(86_400)),
            ..KeepPolicy::default()
        };
        let result = run(&with_keep(fixture.path(), keep)).expect("the run completes");

        assert!(result.entries.is_empty(), "a build from a moment ago is not swept");
        assert!(fixture.path().join("target").exists());
        assert!(matches!(
            result.skips[0].reason,
            SkipReason::KeptByPolicy { rule: "min_age", .. }
        ));
    }

    #[test]
    fn should_hold_an_artifact_below_min_size() {
        let fixture = tree(&["Cargo.toml", "target/debug/app"]);
        let keep = KeepPolicy {
            min_size: Some(1_000_000_000),
            ..KeepPolicy::default()
        };
        let result = run(&with_keep(fixture.path(), keep)).expect("the run completes");

        assert!(result.entries.is_empty());
        assert!(fixture.path().join("target").exists());
        assert!(matches!(
            result.skips[0].reason,
            SkipReason::KeptByPolicy { rule: "min_size", .. }
        ));
    }

    #[test]
    fn should_measure_what_it_removes() {
        let fixture = tree(&["Cargo.toml", "target/debug/app"]);
        let result = run(&options(fixture.path())).expect("the run completes");
        assert!(
            result.entries[0].bytes > 0,
            "a removed artifact reports the space it reclaimed"
        );
        assert_eq!(result.totals().bytes, result.entries[0].bytes);
    }

    #[test]
    fn should_fail_before_scanning_when_a_root_does_not_exist() {
        let mut options = options(std::path::Path::new("/"));
        options.roots = vec![PathBuf::from("/no/such/tree")];
        assert!(run(&options).is_err());
    }

    /// `/tmp` is a symlink to `/private/tmp` on macOS, so naming both walked one tree twice and
    /// reported every byte twice. Found by running voom against a real machine.
    #[test]
    #[cfg(unix)]
    fn should_not_count_one_tree_twice_when_two_roots_resolve_to_it() {
        let fixture = tree(&["real/Cargo.toml", "real/target/app"]);
        std::os::unix::fs::symlink(fixture.path().join("real"), fixture.path().join("link")).expect("a symlink");

        let mut options = options(fixture.path());
        options.dry_run = true;
        options.roots = vec![fixture.path().join("real"), fixture.path().join("link")];

        let result = run(&options).expect("the run completes");

        assert_eq!(result.entries.len(), 1, "one artifact, however many names its root has");
        assert_eq!(
            result.roots,
            vec![fixture.path().join("real")],
            "the spelling typed first is the one kept"
        );
    }

    /// Which spelling survives is not cosmetic: exclusion globs are matched textually against
    /// the walked path, so only the surviving spelling's globs keep working.
    #[test]
    #[cfg(unix)]
    fn should_keep_the_spelling_typed_first_when_two_roots_alias_one_tree() {
        let fixture = tree(&["real/Cargo.toml", "real/target/app"]);
        let real = fixture.path().join("real");
        let link = fixture.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("a symlink");

        for typed in [vec![link.clone(), real.clone()], vec![real.clone(), link.clone()]] {
            let first = typed[0].clone();
            let mut options = options(fixture.path());
            options.dry_run = true;
            options.roots = typed;

            let result = run(&options).expect("the run completes");
            assert_eq!(
                result.roots,
                vec![first],
                "the order the user typed decides, not the spelling"
            );
        }
    }

    /// Sorting is what lets a single pass over the survivors collapse a chain, so the order the
    /// roots arrive in must not matter.
    #[test]
    fn should_collapse_a_chain_of_nested_roots_in_any_order() {
        let fixture = tree(&["a/b/c/Cargo.toml"]);
        let a = fixture.path().join("a");
        let b = fixture.path().join("a/b");
        let c = fixture.path().join("a/b/c");

        for typed in [
            vec![a.clone(), b.clone(), c.clone()],
            vec![c.clone(), b.clone(), a.clone()],
            vec![b.clone(), c.clone(), a.clone()],
        ] {
            assert_eq!(normalize_roots(&typed).expect("the roots resolve"), vec![a.clone()]);
        }
    }

    /// Two roots that share a name prefix but not a parent are not duplicates. `Path::starts_with`
    /// is component-wise, so `a/bc` is not inside `a/b` — this pins that it stays that way.
    #[test]
    fn should_keep_sibling_roots_that_share_a_name_prefix() {
        let fixture = tree(&["b/Cargo.toml", "bc/Cargo.toml"]);
        let roots = vec![fixture.path().join("b"), fixture.path().join("bc")];

        assert_eq!(normalize_roots(&roots).expect("the roots resolve"), roots);
    }

    /// `voom ~ ~/projects` is the same problem without a symlink.
    #[test]
    fn should_drop_a_root_nested_inside_another_root() {
        let fixture = tree(&["outer/inner/Cargo.toml", "outer/inner/target/app"]);
        let mut options = options(fixture.path());
        options.dry_run = true;
        options.roots = vec![fixture.path().join("outer"), fixture.path().join("outer/inner")];

        let result = run(&options).expect("the run completes");

        assert_eq!(result.entries.len(), 1);
        assert_eq!(
            result.roots,
            vec![fixture.path().join("outer")],
            "the path as typed is kept"
        );
    }

    /// `-j` bounded the walk but not the rayon fan-out that sizes and removes, so `-j 2` on a
    /// network filesystem meant a throttled walk followed by a full-core removal storm.
    #[test]
    fn should_bound_the_worker_pool_to_the_requested_jobs() {
        assert_eq!(build_pool(Some(1)).expect("a pool").current_num_threads(), 1);
        assert_eq!(build_pool(Some(3)).expect("a pool").current_num_threads(), 3);
        assert!(
            build_pool(None).expect("a pool").current_num_threads() > 0,
            "no request means available parallelism, not zero threads"
        );
    }

    /// `-j 0` is a plausible typo for "no limit"; it must not mean "no workers".
    #[test]
    fn should_treat_a_request_for_no_threads_as_a_request_for_one() {
        assert_eq!(build_pool(Some(0)).expect("a pool").current_num_threads(), 1);
    }

    /// The thread count alone would be satisfied by a pool nothing runs in, so this checks that
    /// work sent through it is actually held to that width.
    #[test]
    fn should_hold_parallel_work_to_the_width_of_the_pool() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pool = build_pool(Some(2)).expect("a pool");
        let in_flight = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);

        pool.install(|| {
            (0..64).into_par_iter().for_each(|_| {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(2));
                in_flight.fetch_sub(1, Ordering::SeqCst);
            });
        });

        assert!(peak.load(Ordering::SeqCst) <= 2, "the pool did not bound the work");
    }

    /// Throttling changes how fast the work happens, never what it does.
    #[test]
    fn should_sweep_identically_however_many_workers_it_is_given() {
        let paths = &["a/Cargo.toml", "a/target/app", "b/package.json", "b/dist/app.js"];

        let unbounded = tree(paths);
        let throttled = tree(paths);
        let mut throttled_options = options(throttled.path());
        throttled_options.jobs = Some(1);

        let wide = run(&options(unbounded.path())).expect("the run completes");
        let narrow = run(&throttled_options).expect("the run completes");

        assert_eq!(wide.entries.len(), narrow.entries.len());
        assert_eq!(snapshot(unbounded.path()), snapshot(throttled.path()));
    }

    /// The reviewer's case: one root reached through a symlink that lands *inside* another.
    /// It is a duplicate, not a sibling, and the sort has to see that before the prefix filter
    /// can drop it.
    #[test]
    #[cfg(unix)]
    fn should_drop_a_root_that_symlinks_into_another_root() {
        let fixture = tree(&["outer/inner/Cargo.toml", "outer/inner/target/app"]);
        let link = fixture.path().join("link");
        std::os::unix::fs::symlink(fixture.path().join("outer/inner"), &link).expect("a symlink");

        let mut options = options(fixture.path());
        options.dry_run = true;
        options.roots = vec![fixture.path().join("outer"), link];

        let result = run(&options).expect("the run completes");

        assert_eq!(result.entries.len(), 1, "one artifact, reached two ways");
        assert_eq!(result.roots, vec![fixture.path().join("outer")]);
    }

    /// The doc comment promises a bad root stops the run before anything is removed. With one
    /// root that is vacuous; with two it is the property that matters.
    #[test]
    fn should_not_sweep_a_valid_root_when_another_root_does_not_exist() {
        let fixture = tree(&["Cargo.toml", "target/debug/app"]);
        let before = snapshot(fixture.path());

        let mut options = options(fixture.path());
        options.roots = vec![fixture.path().to_path_buf(), PathBuf::from("/no/such/tree")];

        assert!(run(&options).is_err());
        assert_eq!(snapshot(fixture.path()), before, "the good root is left untouched");
    }

    #[test]
    fn should_sweep_several_ecosystems_in_one_pass() {
        let fixture = tree(&[
            "rust/Cargo.toml",
            "rust/target/x",
            "web/package.json",
            "web/dist/x",
            "py/pyproject.toml",
            "py/.mypy_cache/x",
        ]);
        let result = run(&options(fixture.path())).expect("the run completes");
        let mut ecosystems: Vec<_> = result.entries.iter().filter_map(Entry::ecosystem).collect();
        ecosystems.sort_unstable();
        assert_eq!(ecosystems, vec!["node", "python", "rust"]);
    }
}
