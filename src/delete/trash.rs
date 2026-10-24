//! Detached removal: an artifact leaves its tree in one `rename`, and is freed afterwards.
//!
//! Unlinking is the floor every removal stands on, and on a machine whose endpoint-security
//! agents vet each unlink that floor is tens of thousands of entries a second at best — a sweep
//! of Bazel output bases and package caches is tens of millions of entries, and took ninety
//! minutes of the user's time. Nothing about the walk can change that rate; what can change is
//! who waits for it. Once every rail has passed, the artifact is renamed into a trash directory
//! voom owns on the same filesystem — one metadata operation whatever the artifact holds — and a
//! background reaper, started when the run ends, frees the trash at its own pace.
//!
//! The rails are not relaxed by any of this. The rename happens after [`Guard::check`] has
//! returned, on the path it verified, and the reaper removes each trash entry through a
//! [`Guard`] rooted at the trash directory, so every entry it frees has passed the same rails
//! again — strictly below the trash, not a symlink, not protected, same filesystem.
//!
//! An artifact on another filesystem than the trash, or one that contains the trash, is removed
//! in place as before: a `rename` across filesystems is a copy, and a directory cannot be moved
//! into itself.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{Guard, Outcome, Removal, Verified, paths_equal};

/// Overrides where the trash lives. The reaper a run starts is handed the same location.
pub const TRASH_ENV: &str = "VOOM_TRASH_DIR";

/// The reaper's record of what it freed, one line per trash entry.
const LEDGER: &str = ".ledger";

/// Held by the one reaper working the trash at a time.
const LOCK: &str = ".lock";

/// The marker in an entry's name that says it was detached under `--force`.
const FORCED: &str = "f";

/// The marker for an ordinary removal.
const PLAIN: &str = "p";

/// How much of the artifact's own name an entry keeps, for a person looking in the trash.
const NAME_KEPT: usize = 64;

/// Whether removals in this process detach instead of unlinking. Off unless the CLI asks.
static DETACH: AtomicBool = AtomicBool::new(false);

/// Entries this process has detached, which is what decides whether a reaper is started.
static DETACHED: AtomicU64 = AtomicU64::new(0);

/// Distinguishes entries one process detaches within the same nanosecond.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Makes every later removal in this process detach into the trash when it can.
pub fn detach_removals(enabled: bool) {
    DETACH.store(enabled, Ordering::Relaxed);
}

/// How many artifacts this process has detached so far.
#[must_use]
pub fn detached_count() -> u64 {
    DETACHED.load(Ordering::Relaxed)
}

/// Where the trash lives: [`TRASH_ENV`], or `voom/trash` under the platform's cache directory.
#[must_use]
pub fn trash_location() -> Option<PathBuf> {
    match std::env::var_os(TRASH_ENV) {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs::cache_dir().map(|cache| cache.join("voom").join("trash")),
    }
}

/// A trash directory voom owns.
#[derive(Debug)]
pub struct Trash {
    dir: PathBuf,
    #[cfg(unix)]
    dev: u64,
}

impl Trash {
    /// Opens the trash at `dir`, creating it owner-only if it is not there.
    ///
    /// # Errors
    ///
    /// When it cannot be created, or when what is there is not a directory this user owns —
    /// a symlink planted at the trash path must not become somewhere voom moves artifacts to.
    pub fn at(dir: &Path) -> std::io::Result<Self> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
        let metadata = std::fs::symlink_metadata(dir)?;
        if !metadata.is_dir() {
            return Err(std::io::Error::other(format!("{} is not a directory", dir.display())));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // SAFETY: `geteuid` takes nothing and cannot fail.
            let user = unsafe { libc::geteuid() };
            if metadata.uid() != user {
                return Err(std::io::Error::other(format!(
                    "{} belongs to another user",
                    dir.display()
                )));
            }
        }
        Ok(Self {
            dir: dir.canonicalize()?,
            #[cfg(unix)]
            dev: std::os::unix::fs::MetadataExt::dev(&metadata),
        })
    }

    /// The canonical trash directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Moves a verified artifact into the trash.
    ///
    /// `None` when it cannot be detached and must be removed in place: another filesystem, or an
    /// artifact the trash itself sits inside.
    fn detach(&self, verified: &Verified, force: bool) -> Option<std::io::Result<()>> {
        let source = verified.canonical();
        if super::path_starts_with(&self.dir, source) || super::path_starts_with(source, &self.dir) {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            match std::fs::symlink_metadata(source) {
                Ok(metadata) if metadata.dev() == self.dev => {}
                Ok(_) => return None,
                Err(error) => return Some(Err(error)),
            }
        }
        let target = self.dir.join(entry_name(source, force));
        match std::fs::rename(source, &target) {
            Ok(()) => {
                DETACHED.fetch_add(1, Ordering::Relaxed);
                Some(Ok(()))
            }
            // EXDEV: the metadata said one filesystem and the kernel disagreed — a bind mount or
            // a firmlink. The in-place removal is the answer either way.
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => None,
            Err(error) => Some(Err(error)),
        }
    }

    /// Entries waiting to be freed.
    #[must_use]
    pub fn pending(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .collect()
    }
}

/// `<nanos>-<pid>-<sequence>-<f|p>-<name>`: unique without a lookup, ordered by when it was
/// detached, and carrying the one setting the reaper needs — whether the removal was forced.
fn entry_name(source: &Path, force: bool) -> OsString {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name: String = source
        .file_name()
        .map(|name| name.to_string_lossy().chars().take(NAME_KEPT).collect())
        .unwrap_or_default();
    let mode = if force { FORCED } else { PLAIN };
    OsString::from(format!("{nanos}-{}-{sequence}-{mode}-{name}", std::process::id()))
}

/// Whether an entry was detached under `--force`.
fn forced(entry: &Path) -> bool {
    entry
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.split('-').nth(3))
        .is_some_and(|mode| mode == FORCED)
}

/// The process-wide trash, opened once per location.
fn shared() -> Option<std::sync::Arc<Trash>> {
    static TRASH: std::sync::Mutex<Option<(PathBuf, std::sync::Arc<Trash>)>> = std::sync::Mutex::new(None);
    let location = trash_location()?;
    let mut slot = TRASH.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((opened, trash)) = slot.as_ref()
        && paths_equal(opened, &location)
    {
        return Some(std::sync::Arc::clone(trash));
    }
    let trash = std::sync::Arc::new(Trash::at(&location).ok()?);
    *slot = Some((location, std::sync::Arc::clone(&trash)));
    Some(trash)
}

/// Detaches a verified artifact when this process detaches and the artifact can be.
///
/// `None` means remove it in place.
pub(super) fn try_detach(verified: &Verified, removal: Removal) -> Option<Outcome> {
    if !DETACH.load(Ordering::Relaxed) {
        return None;
    }
    let trash = shared()?;
    match trash.detach(verified, removal.force)? {
        Ok(()) => Some(Outcome::Detached),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Outcome::Refused(super::Refusal::Vanished)),
        // A rename that failed for any other reason — a read-only parent, an immutable flag —
        // is answered by the in-place removal, which has `--force` and reports in detail.
        Err(_) => None,
    }
}

/// What a reaper freed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Reaped {
    /// Entries freed completely.
    pub freed_entries: u64,
    /// Bytes freed, including from entries that could only be freed in part.
    pub bytes: u64,
    /// Entries left in the trash because something in them could not be removed.
    pub failed_entries: u64,
}

impl Reaped {
    fn add(&mut self, other: Self) {
        self.freed_entries += other.freed_entries;
        self.bytes += other.bytes;
        self.failed_entries += other.failed_entries;
    }
}

/// Frees everything in the trash, waiting first for any reaper already at work.
///
/// Entries that arrive while it runs are taken too; one that cannot be freed is tried once per
/// reap and left for the next.
///
/// # Errors
///
/// When the trash cannot be opened or locked.
pub fn reap(dir: &Path) -> std::io::Result<Reaped> {
    let trash = Trash::at(dir)?;
    let _lock = lock(trash.dir())?;
    let guard = Guard::new(&[trash.dir().to_path_buf()], true).map_err(std::io::Error::other)?;
    let mut tried = std::collections::HashSet::new();
    let mut total = Reaped::default();
    loop {
        let batch: Vec<PathBuf> = trash
            .pending()
            .into_iter()
            .filter(|entry| tried.insert(entry.clone()))
            .collect();
        if batch.is_empty() {
            return Ok(total);
        }
        let reaped = super::install_removal(|| {
            use rayon::prelude::*;
            batch
                .par_iter()
                .map(|entry| reap_entry(&guard, trash.dir(), entry))
                .reduce(Reaped::default, |mut left, right| {
                    left.add(right);
                    left
                })
        });
        total.add(reaped);
    }
}

/// Frees one entry and records it in the ledger.
fn reap_entry(guard: &Guard, dir: &Path, entry: &Path) -> Reaped {
    let removal = if forced(entry) {
        Removal::forced()
    } else {
        Removal::new()
    };
    let (outcome, bytes) = guard.reclaim(entry, removal);
    let reaped = match outcome {
        Outcome::Removed | Outcome::Refused(super::Refusal::Vanished) => Reaped {
            freed_entries: 1,
            bytes,
            failed_entries: 0,
        },
        Outcome::PartiallyRemoved { freed, .. } => Reaped {
            freed_entries: 0,
            bytes: freed,
            failed_entries: 1,
        },
        _ => Reaped {
            failed_entries: 1,
            ..Reaped::default()
        },
    };
    record(dir, reaped);
    reaped
}

/// Appends one entry's result to the ledger. Opened per line, so a run collecting the ledger by
/// renaming it never loses a line written after the rename.
fn record(dir: &Path, reaped: Reaped) {
    let line = format!(
        "{}\t{}\t{}\n",
        reaped.freed_entries, reaped.bytes, reaped.failed_entries
    );
    if let Ok(mut ledger) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LEDGER))
    {
        let _ = ledger.write_all(line.as_bytes());
    }
}

/// Collects and clears what background reapers have recorded since the last collection.
#[must_use]
pub fn collect_ledger(dir: &Path) -> Reaped {
    let ledger = dir.join(LEDGER);
    let taken = dir.join(format!("{LEDGER}.{}", std::process::id()));
    if std::fs::rename(&ledger, &taken).is_err() {
        return Reaped::default();
    }
    let text = std::fs::read_to_string(&taken).unwrap_or_default();
    let _ = std::fs::remove_file(&taken);
    let mut total = Reaped::default();
    for line in text.lines() {
        let mut fields = line.split('\t').map(|field| field.parse::<u64>().unwrap_or(0));
        total.add(Reaped {
            freed_entries: fields.next().unwrap_or(0),
            bytes: fields.next().unwrap_or(0),
            failed_entries: fields.next().unwrap_or(0),
        });
    }
    total
}

/// Holds the trash's lock until dropped, waiting for a reaper that holds it.
#[cfg(unix)]
fn lock(dir: &Path) -> std::io::Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
    // SAFETY: the descriptor is open for the life of `file`, which the caller holds; `flock` only
    // reads it. The lock goes with the descriptor when `file` is dropped.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(not(unix))]
fn lock(dir: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))
}

/// Starts a background reaper for what this process detached, returning its pid.
///
/// The reaper is its own process group, so the terminal's Ctrl-C reaches the run and not it,
/// and it is handed this process's trash location so both agree on what to free.
///
/// # Errors
///
/// When the binary cannot be located or the process cannot be started.
pub fn spawn_reaper() -> std::io::Result<Option<u32>> {
    if detached_count() == 0 {
        return Ok(None);
    }
    let Some(location) = trash_location() else {
        return Ok(None);
    };
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(["reap", "--background"])
        .env(TRASH_ENV, location)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    Ok(Some(command.spawn()?.id()))
}

/// Lowers this process's scheduling priority, for a reaper that should yield to the user's work.
pub fn yield_to_foreground() {
    #[cfg(unix)]
    // SAFETY: `setpriority` on this process with a constant niceness reads no memory; a failure
    // leaves the priority where it was, which is harmless.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testing::tree;

    #[expect(clippy::expect_used, reason = "a fixture that fails a rail is the failure report")]
    fn verified(guard: &Guard, path: &Path) -> Verified {
        guard.check(path).expect("the fixture passes every rail")
    }

    #[test]
    fn should_move_an_artifact_into_the_trash_in_one_rename_and_free_it_on_reap() {
        let root = tree(&[
            "project/target/debug/a.o",
            "project/target/debug/b.o",
            "project/src/main.rs",
        ]);
        let trash_dir = root.path().join("trash");
        let trash = Trash::at(&trash_dir).unwrap();
        let guard = Guard::new(&[root.path().join("project")], true).unwrap();
        let artifact = root.path().join("project/target");

        assert!(trash.detach(&verified(&guard, &artifact), false).unwrap().is_ok());
        assert!(!artifact.exists(), "the artifact has left its tree");
        assert!(root.path().join("project/src/main.rs").exists(), "the source stays");
        assert_eq!(trash.pending().len(), 1);

        let reaped = reap(&trash_dir).unwrap();
        assert_eq!(reaped.freed_entries, 1);
        assert_eq!(reaped.failed_entries, 0);
        assert!(reaped.bytes > 0, "the reaper counts what it frees");
        assert_eq!(trash.pending(), [] as [std::path::PathBuf; 0]);
        assert_eq!(collect_ledger(&trash_dir), reaped, "the ledger agrees with the reap");
        assert_eq!(
            collect_ledger(&trash_dir),
            Reaped::default(),
            "and is cleared once collected"
        );
    }

    #[test]
    fn should_not_detach_an_artifact_the_trash_is_inside() {
        let root = tree(&["cache/voom/keep"]);
        let trash = Trash::at(&root.path().join("cache/voom/trash")).unwrap();
        let guard = Guard::new(&[root.path().to_path_buf()], true).unwrap();
        assert!(
            trash
                .detach(&verified(&guard, &root.path().join("cache")), false)
                .is_none()
        );
        assert!(root.path().join("cache/voom/keep").exists());
    }

    #[test]
    fn should_refuse_a_trash_that_is_a_symlink() {
        let root = tree(&["elsewhere/precious"]);
        std::os::unix::fs::symlink(root.path().join("elsewhere"), root.path().join("trash")).unwrap();
        assert!(Trash::at(&root.path().join("trash")).is_err());
    }

    #[test]
    fn should_keep_the_force_setting_for_the_reaper() {
        let root = tree(&["project/target/locked/file"]);
        let trash_dir = root.path().join("trash");
        let trash = Trash::at(&trash_dir).unwrap();
        let guard = Guard::new(&[root.path().join("project")], true).unwrap();
        let artifact = root.path().join("project/target");
        assert!(trash.detach(&verified(&guard, &artifact), true).unwrap().is_ok());
        let entry = trash.pending().pop().unwrap();
        assert!(forced(&entry));
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(entry.join("locked"), std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        let reaped = reap(&trash_dir).unwrap();
        assert_eq!(reaped.failed_entries, 0, "a forced entry is repaired and freed");
        assert_eq!(trash.pending(), [] as [std::path::PathBuf; 0]);
    }

    #[test]
    fn should_leave_an_entry_it_cannot_free_and_say_so() {
        use std::os::unix::fs::PermissionsExt;
        let root = tree(&["project/target/locked/file"]);
        let trash_dir = root.path().join("trash");
        let trash = Trash::at(&trash_dir).unwrap();
        let guard = Guard::new(&[root.path().join("project")], true).unwrap();
        assert!(
            trash
                .detach(&verified(&guard, &root.path().join("project/target")), false)
                .unwrap()
                .is_ok()
        );
        let entry = trash.pending().pop().unwrap();
        std::fs::set_permissions(entry.join("locked"), std::fs::Permissions::from_mode(0o500)).unwrap();
        let reaped = reap(&trash_dir).unwrap();
        std::fs::set_permissions(entry.join("locked"), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(reaped.failed_entries, 1);
        assert_eq!(trash.pending().len(), 1, "an unforced entry is not repaired");
    }
}
