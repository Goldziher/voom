//! How wide a worker pool may be when `-j` does not say.
//!
//! voom fans out over rayon for the walk, for sizing and removal, and for the git and docker
//! stages. Each worker holds a handful of descriptors — a directory for each level it has walked
//! into, a subprocess and its pipes — so a pool sized only by the cores will exhaust the
//! descriptor limit on a machine that sets it low, and every removal or spawn then fails with
//! `EMFILE`. macOS gives a process launched from the GUI a soft limit of 256, which a 128-wide
//! pool exceeds; a real sweep over a directory of twenty-seven repositories failed every one of
//! them with `Too many open files` under a limit of 128.
//!
//! An explicit `-j` is honoured as written: it is a decision about the machine, and a user who
//! names a width has taken the trade-off this module exists to take automatically.

/// The default width: eight workers per core, capped at 128, and never more than the descriptor
/// limit can carry.
#[must_use]
pub(crate) fn default_width() -> usize {
    let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    (cores * 8).clamp(8, 128).min(descriptor_cap())
}

/// How many workers the descriptor limit can carry, at a conservative few descriptors each.
///
/// `usize::MAX` when the limit cannot be read, so a caller that only ever takes the minimum is
/// unaffected on a platform without `getrlimit`.
#[must_use]
pub(crate) fn descriptor_cap() -> usize {
    descriptors().map_or(usize::MAX, |limit| (limit / 32).max(1))
}

/// The process's soft descriptor limit, when the platform exposes it.
fn descriptors() -> Option<usize> {
    #[cfg(unix)]
    {
        // SAFETY: `limit` is a plain out-parameter, and `getrlimit` only writes through it.
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } == 0 {
            return usize::try_from(limit.rlim_cur).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the module exists for: on any limit the process can read, a default pool must
    /// leave descriptors for the subprocesses and directories its workers open.
    #[test]
    fn a_default_width_never_exceeds_the_descriptor_cap() {
        let width = default_width();
        assert!(width >= 1, "a pool of zero workers is not a pool");
        assert!(width <= descriptor_cap(), "{width} exceeds the descriptor cap");
        assert!(width <= 128, "{width} exceeds the hard cap");
    }

    /// A low limit must shrink the pool; a high one must not be mistaken for a low one.
    #[test]
    fn the_descriptor_cap_shrinks_with_the_limit() {
        assert_eq!(descriptor_cap_for(128), 4);
        assert_eq!(descriptor_cap_for(256), 8);
        assert_eq!(descriptor_cap_for(1024), 32);
    }

    /// The cap for a hypothetical limit, so the arithmetic is pinned without touching the running
    /// process's real limit.
    fn descriptor_cap_for(limit: usize) -> usize {
        (limit / 32).max(1)
    }
}
