//! Retrying a removal, and repairing what is holding it.
//!
//! `--force` is not a way past a rail. Every function here takes a [`Verified`], and the only
//! thing that mints one is [`Guard::check`](super::Guard::check) — so a permission repair cannot
//! be reached with a path that has not been canonicalized, checked against the denylist, proven
//! strictly below a scan root and proven not to be a symlink. The containment argument is a type
//! argument, and the type already existed.
//!
//! What it does do is answer the two reasons a removal fails that voom can do something about:
//! a directory that was not empty at the moment it was unlinked, which is a question of timing;
//! and something inside the artifact that could not be unlinked, which is a question of modes.
//! Everything else — a read-only mount, an I/O error on a failing disk — is reported rather than
//! retried, because waiting does not change either of them.

use std::path::Path;
use std::time::Duration;

use super::Verified;

/// How a removal behaves once every rail has passed.
///
/// Never affects the rails themselves: this is read after [`Guard::check`](super::Guard::check)
/// has already returned, on a path it has already cleared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Removal {
    /// Withhold the final step, reporting what would have happened.
    pub dry_run: bool,
    /// Retry a failure, repairing permissions inside the artifact first.
    pub force: bool,
}

impl Removal {
    /// A real removal, with no retrying.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A dry run.
    #[must_use]
    pub fn dry() -> Self {
        Self {
            dry_run: true,
            force: false,
        }
    }

    /// A real removal that retries and repairs.
    #[must_use]
    pub fn forced() -> Self {
        Self {
            dry_run: false,
            force: true,
        }
    }
}

/// How long to wait before each retry.
///
/// Four attempts in a little over a second. That is enough to outlast a build that has just
/// finished writing, and deliberately not enough to outlast one that is still going — voom must
/// report and move on rather than sit on a worker waiting for something that may take minutes.
const BACKOFF: [Duration; 3] = [
    Duration::from_millis(50),
    Duration::from_millis(200),
    Duration::from_millis(800),
];

/// Removes a verified path, retrying while `--force` and the kind of failure both warrant it.
///
/// Returns the last error, so the caller decides between a total failure and a partial one.
///
/// The sleeps block a rayon worker. That is bounded by [`BACKOFF`] and by how rare a failure is,
/// and it only happens under `--force`, but it is a real cost on a tree with many held
/// artifacts and a low `-j`.
pub(super) fn remove(verified: &Verified, removal: Removal) -> std::io::Result<()> {
    let mut repaired = false;
    let mut attempt = 0;

    loop {
        let result = if verified.is_dir {
            std::fs::remove_dir_all(verified.canonical())
        } else {
            std::fs::remove_file(verified.canonical())
        };

        let Err(error) = result else { return Ok(()) };
        if !removal.force {
            return Err(error);
        }

        match error.kind() {
            // A writer got there between voom emptying a directory and unlinking it. Time is
            // the only thing that helps, and a mode change would be answering a question nobody
            // asked.
            std::io::ErrorKind::DirectoryNotEmpty => {
                let Some(wait) = BACKOFF.get(attempt) else {
                    return Err(error);
                };
                std::thread::sleep(*wait);
                attempt += 1;
            }
            // Modes do not change by waiting, so repair once and retry immediately. A second
            // denial means the obstruction is outside the artifact — most often the artifact's
            // own parent — which `--force` deliberately will not touch.
            std::io::ErrorKind::PermissionDenied if !repaired => {
                repair(verified.canonical());
                repaired = true;
            }
            _ => return Err(error),
        }
    }
}

/// Clears what stops an unlink *inside* an artifact that has already passed every rail.
///
/// Bounded to the artifact by construction: the walk starts at `root`, never follows a symlink,
/// and builds every path by joining a `read_dir` entry's own file name onto a directory already
/// inside it — so it cannot climb out. It never touches `root`'s parent, which is why an
/// artifact held by a read-only *parent* is reported rather than reclaimed. [`relax`] adds the
/// one bound the walk cannot: a hard-linked file names an inode that is also reachable from
/// outside, and is left alone.
///
/// That bound holds against the filesystem as it is, not against one being rewritten underneath.
/// The walk checks a path with `symlink_metadata` and then names it again to `read_dir` and
/// `set_permissions`, and `std` offers no `openat`-style way to close the gap — so a writer that
/// replaces a checked directory with a symlink before it is dequeued can be followed. It is the
/// same race `remove_dir_all` itself carries, worth naming here only because this code changes
/// modes rather than only unlinking.
///
/// Failures are ignored per path. A repair that could not be applied is not a new error to
/// report; the retry that follows is the only thing that decides.
fn repair(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    // The artifact's own mode governs unlinking its children, so it is repaired too.
    relax(root);

    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // `symlink_metadata`, never `metadata`: a symlink's own mode has no bearing on
            // unlinking it, and following one is how a repair would escape the artifact.
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            relax(&path);
            if metadata.is_dir() {
                stack.push(path);
            }
        }
    }
}

/// Makes one path unlinkable, preserving every permission bit the removal does not need.
#[cfg(unix)]
fn relax(path: &Path) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    // A mode bit and a BSD flag both belong to the inode; a directory entry is only a name for
    // it. Removal unlinks the one name inside the artifact, but a repair would widen the file
    // everywhere it is named — including a hard link into a tree voom was never given. A
    // directory cannot be hard-linked, so this only ever skips a file, and skipping one means
    // at worst a failure reported instead of reclaimed.
    if !metadata.is_dir() && metadata.nlink() > 1 {
        return;
    }
    let mode = metadata.permissions().mode();
    // On a directory: read to list its entries, write to unlink them, execute to reach them —
    // a directory missing any of the three cannot be emptied, and a search-only `0o111` is the
    // shape that made `--force` a no-op on exactly the case it exists for. On a file: write,
    // which unlink does not consult on any filesystem here but costs nothing to grant.
    //
    // The owner's bits only — widening group or other would leave the tree more open than voom
    // found it if the removal then failed anyway.
    let wanted = if metadata.is_dir() { 0o700 } else { 0o200 };
    if mode & wanted != wanted {
        let _ = std::fs::set_permissions(path, PermissionsExt::from_mode(mode | wanted));
    }
    clear_immutable(path, &metadata);
}

#[cfg(not(unix))]
fn relax(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    let mut permissions = metadata.permissions();
    if permissions.readonly() {
        #[expect(clippy::permissions_set_readonly_false, reason = "clearing it is the point")]
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

/// Clears the BSD user-immutable flags, which stop an unlink regardless of mode.
///
/// `st_flags` can be read through `std`, but nothing in `std` can set them, so this is the one
/// FFI call in the crate. The system-immutable flags are deliberately left alone: clearing them
/// needs root and, at a raised securelevel, cannot be done at all — an artifact carrying one is
/// reported rather than forced.
#[cfg(target_vendor = "apple")]
fn clear_immutable(path: &Path, metadata: &std::fs::Metadata) {
    use std::ffi::CString;
    use std::os::darwin::fs::MetadataExt;
    use std::os::unix::ffi::OsStrExt;

    const UF_IMMUTABLE: u32 = 0x0000_0002;
    const UF_APPEND: u32 = 0x0000_0004;

    let flags = metadata.st_flags();
    if flags & (UF_IMMUTABLE | UF_APPEND) == 0 {
        return;
    }
    let Ok(raw) = CString::new(path.as_os_str().as_bytes()) else {
        return;
    };
    // SAFETY: `raw` is a NUL-terminated C string that outlives the call, and `chflags` neither
    // retains nor frees the pointer. The return value is deliberately ignored — a repair that
    // could not be applied is not an error, and the retry decides.
    unsafe {
        libc::chflags(raw.as_ptr(), flags & !(UF_IMMUTABLE | UF_APPEND));
    }
}

/// Every unix that is not Apple: `std` exposes no BSD flags to clear, and there is nothing else
/// a mode bit has not already covered.
#[cfg(all(unix, not(target_vendor = "apple")))]
fn clear_immutable(_path: &Path, _metadata: &std::fs::Metadata) {}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::delete::{Guard, Outcome, PROTECTED_PATHS};
    use crate::testing::tree;
    // Only the dry-run test compares whole trees, and it is unix-only — every other case here
    // asserts on a mode or a path.
    #[cfg(unix)]
    use crate::testing::snapshot;

    fn guard(root: &Path) -> Guard {
        Guard::new(&[root.to_path_buf()], true).expect("the fixture root resolves")
    }

    /// The ladder is a policy, and a policy nobody can see is one that grows silently. Roughly a
    /// second is the budget: long enough to outlast a build that has just finished, short enough
    /// that a tree full of held artifacts does not turn a sweep into a wait.
    #[test]
    fn the_retry_ladder_is_bounded() {
        let total: Duration = BACKOFF.iter().sum();

        assert!(
            total <= Duration::from_millis(1_100),
            "the retries add up to {total:?} per artifact, which is no longer a short backoff"
        );
        assert!(
            BACKOFF.windows(2).all(|pair| pair[0] < pair[1]),
            "each wait must be longer than the last"
        );
    }

    /// `--force` is a setting on what happens *after* the rails, so its default has to be the
    /// behaviour that existed before it did.
    #[test]
    fn a_removal_forces_nothing_unless_asked() {
        assert!(!Removal::new().force);
        assert!(!Removal::new().dry_run);
        assert!(!Removal::dry().force, "a dry run never repairs anything");
        assert!(Removal::forced().force);
        assert!(!Removal::forced().dry_run);
    }

    /// `--force` never reaches a rail, and this is the test that keeps it true as rails are
    /// added. It runs the refusals through [`Guard::remove`] rather than [`Guard::check`],
    /// because `check` does not take a [`Removal`] at all — the rails are decided before the
    /// setting is read, and that structure is what the assertion is really pinning.
    #[test]
    fn force_should_still_refuse_every_rail() {
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("precious.txt"), b"not voom's business").unwrap();

        let fixture = tree(&["Cargo.toml", ".git/HEAD", "target/app"]);
        let guard = guard(fixture.path());

        let mut refusable: Vec<std::path::PathBuf> = vec![
            fixture.path().join(".git"),  // the VCS rail
            outside.path().to_path_buf(), // containment
            fixture.path().to_path_buf(), // the scan root itself
        ];
        refusable.extend(
            PROTECTED_PATHS
                .iter()
                .map(Path::new)
                .filter(|path| path.exists())
                .map(Path::to_path_buf),
        );

        for path in refusable {
            for removal in [Removal::new(), Removal::forced()] {
                let outcome = guard.remove(&path, 0, removal);
                assert!(
                    matches!(outcome, Outcome::Refused(_)),
                    "`{}` was not refused with force={}: {outcome:?}",
                    path.display(),
                    removal.force
                );
            }
        }

        assert!(
            outside.path().join("precious.txt").exists(),
            "and nothing outside the root was touched by either pass"
        );
    }

    /// The case `--force` exists for: something inside the artifact cannot be unlinked, so the
    /// removal stops with real bytes still on disk. Plain mode reports it; forced mode repairs
    /// the modes *inside* the artifact and finishes the job.
    #[test]
    #[cfg(unix)]
    fn force_should_reclaim_an_artifact_a_read_only_subdirectory_was_holding() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tree(&["Cargo.toml", "target/locked/held.bin"]);
        let locked = fixture.path().join("target/locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let guard = guard(fixture.path());
        let target = fixture.path().join("target");

        let plain = guard.remove(&target, 0, Removal::new());
        if matches!(plain, Outcome::Removed) {
            // Running as root defeats the permission bits entirely.
            return;
        }
        assert!(target.exists(), "plain mode leaves it: {plain:?}");

        let forced = guard.remove(&target, 0, Removal::forced());

        assert_eq!(forced, Outcome::Removed, "forced mode finishes it");
        assert!(!target.exists());
    }

    /// Without the flag, nothing is repaired. A sweep must leave the modes it found.
    #[test]
    #[cfg(unix)]
    fn should_not_repair_permissions_without_force() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tree(&["Cargo.toml", "target/locked/held.bin"]);
        let locked = fixture.path().join("target/locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let before = std::fs::symlink_metadata(&locked).unwrap().permissions().mode();

        let outcome = guard(fixture.path()).remove(&fixture.path().join("target"), 0, Removal::new());
        if matches!(outcome, Outcome::Removed) {
            return;
        }

        assert_eq!(
            std::fs::symlink_metadata(&locked).unwrap().permissions().mode(),
            before,
            "an unforced run changes no mode"
        );
    }

    /// The containment test for the repair. A symlink inside an artifact must not become a route
    /// to chmod'ing whatever it points at — which for a link pointing outside the scan root
    /// would be a change to a tree voom was never given.
    #[test]
    #[cfg(unix)]
    fn force_should_not_follow_a_symlink_when_repairing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let outside = tempfile::TempDir::new().unwrap();
        let guarded = outside.path().join("guarded");
        std::fs::create_dir_all(&guarded).unwrap();
        std::fs::write(guarded.join("precious.rs"), b"fn main() {}").unwrap();
        std::fs::set_permissions(&guarded, std::fs::Permissions::from_mode(0o555)).unwrap();
        let before = std::fs::symlink_metadata(&guarded).unwrap().permissions().mode();

        let fixture = tree(&["Cargo.toml", "target/app"]);
        std::os::unix::fs::symlink(&guarded, fixture.path().join("target/link")).unwrap();

        let _ = guard(fixture.path()).remove(&fixture.path().join("target"), 0, Removal::forced());

        assert_eq!(
            std::fs::symlink_metadata(&guarded).unwrap().permissions().mode(),
            before,
            "the link's target keeps its mode: the repair never followed it"
        );
        assert!(guarded.join("precious.rs").exists(), "and keeps its contents");
        std::fs::set_permissions(&guarded, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `--force` is bounded to the artifact, so it cannot reach the artifact's parent — which is
    /// why an artifact held by a read-only parent is reported rather than reclaimed. Documenting
    /// the limit is the point of the test.
    #[test]
    #[cfg(unix)]
    fn force_should_not_change_anything_outside_the_artifact() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tree(&["project/Cargo.toml", "project/target/app", "project/src/main.rs"]);
        let project = fixture.path().join("project");
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o555)).unwrap();
        let parent_before = std::fs::symlink_metadata(&project).unwrap().permissions().mode();
        let sibling_before = std::fs::symlink_metadata(project.join("src"))
            .unwrap()
            .permissions()
            .mode();

        let _ = guard(fixture.path()).remove(&project.join("target"), 0, Removal::forced());

        assert_eq!(
            std::fs::symlink_metadata(&project).unwrap().permissions().mode(),
            parent_before,
            "the artifact's parent is never touched"
        );
        assert_eq!(
            std::fs::symlink_metadata(project.join("src"))
                .unwrap()
                .permissions()
                .mode(),
            sibling_before,
            "nor is a sibling"
        );
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A directory with no read bit cannot be listed, so it cannot be emptied — and `0o111` is
    /// exactly the shape a build tool produces when it means "search only". The repair granted
    /// write and execute and not read until this test existed, which made `--force` a no-op on
    /// one of the two failures it was written to answer.
    #[test]
    #[cfg(unix)]
    fn force_should_reclaim_an_artifact_a_search_only_directory_was_holding() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tree(&["Cargo.toml", "target/sealed/held.bin"]);
        let sealed = fixture.path().join("target/sealed");
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o111)).unwrap();
        let guard = guard(fixture.path());
        let target = fixture.path().join("target");

        let plain = guard.remove(&target, 0, Removal::new());
        if matches!(plain, Outcome::Removed) {
            // Running as root defeats the permission bits entirely.
            return;
        }
        assert!(target.exists(), "plain mode leaves it: {plain:?}");

        assert_eq!(
            guard.remove(&target, 0, Removal::forced()),
            Outcome::Removed,
            "forced mode grants read as well and finishes it"
        );
        assert!(!target.exists());
    }

    /// The containment test the mode bits need that the path walk cannot give them. A hard link
    /// is a second name for one inode, so relaxing a file inside the artifact would widen the
    /// data wherever else it is named — while the removal only ever unlinks the name inside.
    /// Leaving it alone costs at most a failure reported instead of reclaimed.
    #[test]
    #[cfg(unix)]
    fn force_should_not_widen_a_file_hard_linked_from_outside_the_artifact() {
        use std::os::unix::fs::PermissionsExt;

        let outside = tempfile::TempDir::new().unwrap();
        let precious = outside.path().join("precious.rs");
        std::fs::write(&precious, b"fn main() {}").unwrap();
        std::fs::set_permissions(&precious, std::fs::Permissions::from_mode(0o400)).unwrap();

        let fixture = tree(&["Cargo.toml", "target/locked/app"]);
        std::fs::hard_link(&precious, fixture.path().join("target/shared.rs")).unwrap();
        let locked = fixture.path().join("target/locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

        let _ = guard(fixture.path()).remove(&fixture.path().join("target"), 0, Removal::forced());

        assert_eq!(
            std::fs::symlink_metadata(&precious).unwrap().permissions().mode() & 0o777,
            0o400,
            "the inode's mode is unchanged where it is named outside the artifact"
        );
    }

    /// A dry run withholds the final step, and `--force` lives entirely after it. Accepting both
    /// together has to be a strict no-op rather than an error, because a hook alias carrying both
    /// must keep working.
    #[test]
    #[cfg(unix)]
    fn a_dry_run_with_force_touches_nothing() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tree(&["Cargo.toml", "target/locked/held.bin"]);
        let locked = fixture.path().join("target/locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let before = snapshot(fixture.path());
        let mode = std::fs::symlink_metadata(&locked).unwrap().permissions().mode();

        let outcome = guard(fixture.path()).remove(
            &fixture.path().join("target"),
            0,
            Removal {
                dry_run: true,
                force: true,
            },
        );

        assert_eq!(outcome, Outcome::WouldRemove);
        assert_eq!(snapshot(fixture.path()), before, "the tree is byte-identical");
        assert_eq!(
            std::fs::symlink_metadata(&locked).unwrap().permissions().mode(),
            mode,
            "and no mode was repaired"
        );
    }
}
