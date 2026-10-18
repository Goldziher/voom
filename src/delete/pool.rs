//! The process-wide pool that removals run in, and the byte counters a progress line reads.
//!
//! Split out of `delete/mod.rs` to keep that module under the size cap. Removal has its own
//! width, its own worker threads and its own accounting — a concern distinct from the rails the
//! parent module exists to run.

use super::{Removal, Verified, force};

/// Bytes every removal in the process has freed so far, for a progress line.
static FREED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Records bytes a removal has just freed.
pub(crate) fn note_freed(bytes: u64) {
    FREED.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
}

/// Bytes every removal in this process has freed so far. Only ever grows; a progress line reads
/// it while removals run, and nothing decides anything by it.
#[must_use]
pub fn freed_so_far() -> u64 {
    FREED.load(std::sync::atomic::Ordering::Relaxed)
}

/// A shared mutation width chosen from measurements on macOS cache trees.
#[cfg(target_vendor = "apple")]
const REMOVAL_WORKERS: usize = 8;

/// Keep the existing default on platforms without these cache measurements.
#[cfg(not(target_vendor = "apple"))]
const REMOVAL_WORKERS: usize = 16;

/// An explicit `-j`, recorded before the first removal; zero means none was given.
static REMOVAL_JOBS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Holds removal to `-j` workers instead of [`REMOVAL_WORKERS`].
///
/// The pool is rebuilt if the width changes, so a second run in the same process — a library
/// caller or the test suite — is not stuck with the first run's `-j`.
pub fn limit_removal_workers(jobs: Option<usize>) {
    REMOVAL_JOBS.store(jobs.map_or(0, |jobs| jobs.max(1)), std::sync::atomic::Ordering::Relaxed);
}

/// The one pool every removal runs in, so that artifacts removed side by side share its width
/// rather than each fanning out across the sizing pool.
///
/// Keyed on the requested width rather than built once: a `OnceLock` kept the first width for the
/// life of the process, so a later `-j` was silently ignored.
fn removal_pool() -> Option<std::sync::Arc<rayon::ThreadPool>> {
    static POOL: std::sync::Mutex<Option<(usize, std::sync::Arc<rayon::ThreadPool>)>> = std::sync::Mutex::new(None);
    let jobs = match REMOVAL_JOBS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => REMOVAL_WORKERS.min(crate::limits::descriptor_cap()),
        jobs => jobs,
    };
    let mut slot = POOL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((width, pool)) = slot.as_ref()
        && *width == jobs
    {
        return Some(std::sync::Arc::clone(pool));
    }
    let pool = std::sync::Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .thread_name(|index| format!("voom-remove-{index}"))
            // Marks every removal worker so `remove_in_pool` can tell it is already inside the
            // pool and must not park a second worker installing into it again.
            .start_handler(|_| IN_REMOVAL_POOL.with(|flag| flag.set(true)))
            .build()
            .ok()?,
    );
    *slot = Some((jobs, std::sync::Arc::clone(&pool)));
    Some(pool)
}

thread_local! {
    /// Whether the current thread is a worker of the removal pool.
    static IN_REMOVAL_POOL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `work` inside the removal pool, so a fan-out over artifacts uses its workers directly
/// instead of one ambient worker per artifact parked in an `install`.
pub(crate) fn install_removal<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    match removal_pool() {
        Some(pool) => pool.install(work),
        None => work(),
    }
}

/// Runs one verified removal in the removal pool.
pub(crate) fn remove_in_pool(verified: &Verified, removal: Removal) -> (std::io::Result<()>, Option<u64>) {
    // Already on a removal worker (the caller installed a fan-out): installing again would park a
    // second thread to do the same work.
    if IN_REMOVAL_POOL.with(std::cell::Cell::get) {
        return force::remove(verified, removal);
    }
    install_removal(|| force::remove(verified, removal))
}
