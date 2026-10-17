//! Measuring and removing whole trees through directory descriptors.
//!
//! Sizing and removal are the stages that touch every entry of every artifact, and on a machine
//! with an endpoint-security agent every `open` is a round trip through it. Measured over Bazel
//! output bases of one to three and a half million entries each, under two such agents: `std`'s
//! path-based `read_dir` plus an `fstatat` per entry on eighteen threads ran at 38,000 entries a
//! second cold; descriptor-relative opens with `getattrlistbulk(2)` on sixty-four ran at 119,000.
//!
//! Three things carry that, and each is a system primitive rather than a cleverness:
//!
//! - every directory is opened with `openat` against its parent's descriptor, so the kernel
//!   resolves one component rather than the whole path again;
//! - on Apple platforms a directory's names, types, sizes, link counts and inodes come back in
//!   bulk, one call per buffer rather than one `fstatat` per entry ([`sys`]);
//! - the walk fans out across `rayon` *inside* one tree, because a sweep's trees are wildly
//!   uneven and the largest would otherwise run on one thread.
//!
//! Removal is the same walk with `unlinkat` in it, post-order, and it counts what it frees as it
//! goes — so a real run never has to measure a tree before taking it.
//!
//! Neither walk follows a symlink or crosses onto another filesystem: a directory is opened
//! `O_NOFOLLOW`, a symlink is unlinked as the link it is, and a directory with something mounted
//! on it is left in place and reported.

mod sys;

use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rayon::prelude::*;

use crate::size::{Links, Measured};
use sys::{Kind, USER_IMMUTABLE};

/// How many levels of a tree are divided across `rayon` before the walk goes serial.
///
/// The bound is about stack depth rather than tuning — see `size::FANOUT_DEPTH`, which this
/// mirrors. Below it the walk holds one descriptor per level, never one per directory seen.
const FANOUT_DEPTH: u32 = 10;

/// The owner bits a directory needs to be listed and emptied.
const DIRECTORY_BITS: u32 = 0o700;

/// Waits between attempts to open a directory while the process is out of descriptors.
const EXHAUSTED_BACKOFF: [Duration; 4] = [
    Duration::from_millis(1),
    Duration::from_millis(5),
    Duration::from_millis(25),
    Duration::from_millis(100),
];

/// Opens `name` under `parent`, waiting briefly if the process has run out of descriptors.
///
/// Every worker holds one descriptor per level it is inside, so a wide fan-out over a deep tree
/// can briefly meet the limit; whichever sibling finishes first hands some back.
fn open(parent: RawFd, name: &CStr) -> std::io::Result<OwnedFd> {
    let mut waits = EXHAUSTED_BACKOFF.iter();
    loop {
        match sys::open_dir_at(parent, name) {
            Err(error) if matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)) => {
                let Some(wait) = waits.next() else { return Err(error) };
                std::thread::sleep(*wait);
            }
            result => return result,
        }
    }
}

/// The recursive size of a directory's contents, below the directory itself.
///
/// The directory's own bytes are the caller's to count, from whatever `stat` it already holds.
pub(crate) fn measure_below(path: &Path) -> Measured {
    let Ok(fd) = sys::open_dir(path) else {
        return Measured {
            bytes: 0,
            unreadable: 1,
        };
    };
    let Ok(own) = sys::stat_fd(fd.as_raw_fd()) else {
        return Measured {
            bytes: 0,
            unreadable: 1,
        };
    };
    measure_open(&fd, own.dev, FANOUT_DEPTH, &Links::default())
}

/// One directory's entries counted, and the subdirectories still to visit.
fn tally(fd: &OwnedFd, dev: u64, seen: &Links) -> (Measured, Vec<CString>) {
    let Ok(listing) = sys::list(fd) else {
        return (
            Measured {
                bytes: 0,
                unreadable: 1,
            },
            Vec::new(),
        );
    };
    let mut total = Measured {
        bytes: 0,
        unreadable: listing.unreadable,
    };
    let mut subdirectories = Vec::new();
    for entry in listing.entries {
        if !seen.counted_inode(entry.dev, entry.ino, entry.nlink) {
            total.bytes += entry.bytes;
        }
        // Another filesystem's contents are not this tree's, and its mount point is what
        // `--one-file-system` exists to stop at.
        if entry.kind == Kind::Dir && !entry.mount_point && entry.dev == dev {
            subdirectories.push(entry.name);
        }
    }
    (total, subdirectories)
}

fn measure_open(fd: &OwnedFd, dev: u64, fanout: u32, seen: &Links) -> Measured {
    let (total, subdirectories) = tally(fd, dev, seen);
    if fanout == 0 {
        return total + measure_serially(fd, subdirectories, dev, seen);
    }
    total
        + subdirectories
            .par_iter()
            .map(|name| match open(fd.as_raw_fd(), name) {
                Ok(child) => measure_open(&child, dev, fanout - 1, seen),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Measured::default(),
                Err(_) => Measured {
                    bytes: 0,
                    unreadable: 1,
                },
            })
            .reduce(Measured::default, |left, right| left + right)
}

/// Everything below `names`, on one thread, with an explicit stack of open descriptors.
fn measure_serially(fd: &OwnedFd, names: Vec<CString>, dev: u64, seen: &Links) -> Measured {
    let mut total = Measured::default();
    let mut stack: Vec<(OwnedFd, Vec<CString>)> = Vec::new();
    let mut first = names;
    loop {
        let (parent, next) = if let Some((dir, pending)) = stack.last_mut() {
            let Some(next) = pending.pop() else {
                stack.pop();
                continue;
            };
            (dir.as_raw_fd(), next)
        } else {
            let Some(next) = first.pop() else { return total };
            (fd.as_raw_fd(), next)
        };
        match open(parent, &next) {
            Ok(child) => {
                let (measured, subdirectories) = tally(&child, dev, seen);
                total += measured;
                stack.push((child, subdirectories));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => total.unreadable += 1,
        }
    }
}

/// What a removal took, and the first thing that stopped it taking the rest.
#[derive(Debug, Default)]
pub(crate) struct Removed {
    /// Bytes unlinked, counted the way [`measure_below`] counts them.
    pub(crate) freed: u64,
    /// The first failure. The walk does not stop at it: everything else that can go, goes.
    pub(crate) error: Option<std::io::Error>,
}

/// Removes the directory at `path` and everything below it.
///
/// `repair` is `--force`: an entry that cannot be unlinked because its directory is read-only,
/// or carries a user-immutable flag, has the owner's bits restored and is retried. Only ever
/// below `path` and on `path` itself — never its parent, so an artifact held by a read-only
/// parent is reported rather than taken — and never on a file another hard link also names.
pub(crate) fn remove_dir(path: &Path, repair: bool) -> Removed {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Removed {
            freed: 0,
            error: Some(std::io::Error::from(std::io::ErrorKind::InvalidInput)),
        };
    };
    let result = (|| {
        use std::os::unix::ffi::OsStrExt;
        let parent = sys::open_dir(parent)?;
        let name = CString::new(name.as_bytes())?;
        let own = sys::stat_at(parent.as_raw_fd(), &name)?;
        if own.kind != Kind::Dir {
            return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
        }
        let remover = Remover {
            repair,
            dev: own.dev,
            seen: Links::default(),
            freed: AtomicU64::new(0),
            error: Mutex::new(None),
        };
        let root = Sub {
            name,
            bytes: own.bytes,
            flags: own.flags,
        };
        remover.remove(parent.as_raw_fd(), root, false, FANOUT_DEPTH);
        Ok(remover.finish())
    })();
    result.unwrap_or_else(|error| Removed {
        freed: 0,
        error: Some(error),
    })
}

/// A subdirectory still to remove, with what its parent's listing said about it.
#[derive(Debug)]
struct Sub {
    name: CString,
    bytes: u64,
    flags: u32,
}

/// A directory open for removal: its descriptor, its name in its parent, and what is left.
struct Frame {
    fd: OwnedFd,
    sub: Sub,
    pending: Vec<Sub>,
}

struct Remover {
    repair: bool,
    dev: u64,
    seen: Links,
    freed: AtomicU64,
    error: Mutex<Option<std::io::Error>>,
}

/// Whether an error is the kind a repair can answer.
fn denied(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EACCES | libc::EPERM))
}

impl Remover {
    fn finish(self) -> Removed {
        Removed {
            freed: self.freed.into_inner(),
            error: self
                .error
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }

    /// Keeps the first failure; later ones are usually its consequences (a parent that is not
    /// empty because a child could not go).
    fn fail(&self, error: std::io::Error) {
        if let Ok(mut slot) = self.error.lock()
            && slot.is_none()
        {
            *slot = Some(error);
        }
    }

    fn count(&self, dev: u64, ino: u64, nlink: u64, bytes: u64) {
        if !self.seen.counted_inode(dev, ino, nlink) {
            self.freed.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    /// Restores the owner's bits on an open directory and clears its user-immutable flags.
    fn repair_dir(fd: RawFd) {
        if let Ok(own) = sys::stat_fd(fd) {
            sys::add_mode_fd(fd, own.mode, DIRECTORY_BITS);
            sys::clear_immutable_fd(fd, own.flags);
        }
    }

    /// Removes one directory below `parent`, fanning out over its subdirectories.
    ///
    /// `parent_repairable` is false only for the artifact itself, whose parent is outside it.
    fn remove(&self, parent: RawFd, sub: Sub, parent_repairable: bool, fanout: u32) {
        let Some(frame) = self.open_and_empty(parent, sub) else {
            return;
        };
        let Frame { fd, sub, pending } = frame;
        if fanout == 0 {
            self.remove_serially(&fd, pending);
        } else {
            pending
                .into_par_iter()
                .for_each(|child| self.remove(fd.as_raw_fd(), child, true, fanout - 1));
        }
        drop(fd);
        self.remove_empty(parent, &sub, parent_repairable);
    }

    /// Post-order removal on one thread, holding one descriptor per level.
    fn remove_serially(&self, fd: &OwnedFd, subdirectories: Vec<Sub>) {
        let mut first = subdirectories;
        let mut stack: Vec<Frame> = Vec::new();
        loop {
            let next = match stack.last_mut() {
                Some(frame) => frame.pending.pop().map(|next| (frame.fd.as_raw_fd(), next)),
                None => match first.pop() {
                    Some(next) => Some((fd.as_raw_fd(), next)),
                    None => return,
                },
            };
            if let Some((parent, sub)) = next {
                if let Some(frame) = self.open_and_empty(parent, sub) {
                    stack.push(frame);
                }
                continue;
            }
            let Some(Frame { fd: done, sub, .. }) = stack.pop() else {
                return;
            };
            drop(done);
            let parent = stack.last().map_or(fd.as_raw_fd(), |frame| frame.fd.as_raw_fd());
            self.remove_empty(parent, &sub, true);
        }
    }

    /// Opens a directory and unlinks every non-directory in it, returning what is left to do.
    fn open_and_empty(&self, parent: RawFd, sub: Sub) -> Option<Frame> {
        let fd = match open(parent, &sub.name) {
            Ok(fd) => fd,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) if self.repair && denied(&error) => {
                // A directory missing its owner's read or search bit cannot be opened to be
                // emptied. Its mode belongs to it, inside the artifact, so it is restored.
                if let Ok(own) = sys::stat_at(parent, &sub.name)
                    && own.kind == Kind::Dir
                {
                    sys::add_mode_at(parent, &sub.name, own.mode, DIRECTORY_BITS);
                }
                match open(parent, &sub.name) {
                    Ok(fd) => fd,
                    Err(error) => {
                        self.fail(error);
                        return None;
                    }
                }
            }
            Err(error) => {
                self.fail(error);
                return None;
            }
        };

        let listing = match sys::list(&fd) {
            Ok(listing) => listing,
            Err(error) => {
                self.fail(error);
                return None;
            }
        };
        if listing.unreadable > 0 {
            self.fail(std::io::Error::other(format!(
                "{} entries could not be read",
                listing.unreadable
            )));
        }

        let mut repaired = false;
        let mut pending = Vec::new();
        for entry in listing.entries {
            if entry.kind == Kind::Dir {
                if entry.mount_point || entry.dev != self.dev {
                    self.fail(std::io::Error::other(format!(
                        "{} is a mount point; voom does not remove across filesystems",
                        entry.name.to_string_lossy()
                    )));
                    continue;
                }
                pending.push(Sub {
                    name: entry.name,
                    bytes: entry.bytes,
                    flags: entry.flags,
                });
                continue;
            }
            match self.unlink(&fd, &entry, &mut repaired) {
                Ok(()) => self.count(entry.dev, entry.ino, entry.nlink, entry.bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => self.fail(error),
            }
        }
        Some(Frame { fd, sub, pending })
    }

    /// Unlinks one non-directory, repairing its directory once if `--force` allows it.
    fn unlink(&self, dir: &OwnedFd, entry: &sys::Entry, repaired: &mut bool) -> std::io::Result<()> {
        let error = match sys::unlink_at(dir.as_raw_fd(), &entry.name, false) {
            Err(error) if self.repair && denied(&error) => error,
            result => return result,
        };
        if !*repaired {
            *repaired = true;
            Self::repair_dir(dir.as_raw_fd());
            match sys::unlink_at(dir.as_raw_fd(), &entry.name, false) {
                Err(error) if denied(&error) => {}
                result => return result,
            }
        }
        // A flag belongs to the inode, which a second hard link names from somewhere voom was
        // never given; clearing it there would widen a file outside the artifact.
        if entry.flags & USER_IMMUTABLE != 0 && entry.nlink <= 1 {
            sys::clear_immutable_at(dir.as_raw_fd(), &entry.name, entry.flags);
            return sys::unlink_at(dir.as_raw_fd(), &entry.name, false);
        }
        Err(error)
    }

    /// Removes a directory that has just been emptied.
    fn remove_empty(&self, parent: RawFd, sub: &Sub, parent_repairable: bool) {
        let result = match sys::unlink_at(parent, &sub.name, true) {
            Err(error) if self.repair && denied(&error) => {
                // The directory's own immutable flag stops its removal as surely as its
                // parent's mode does; the first is inside the artifact whatever the parent is.
                if sub.flags & USER_IMMUTABLE != 0
                    && let Ok(fd) = open(parent, &sub.name)
                {
                    sys::clear_immutable_fd(fd.as_raw_fd(), sub.flags);
                }
                if parent_repairable {
                    Self::repair_dir(parent);
                }
                sys::unlink_at(parent, &sub.name, true)
            }
            result => result,
        };
        match result {
            Ok(()) => self.freed.fetch_add(sub.bytes, Ordering::Relaxed),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => {
                self.fail(error);
                0
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::testing::tree;

    fn std_measure(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        let mut total = 0;
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let metadata = entry.metadata().unwrap();
                if metadata.nlink() > 1 && !metadata.is_dir() && !seen.insert(metadata.ino()) {
                    continue;
                }
                total += metadata.blocks() * 512;
                if metadata.is_dir() {
                    stack.push(entry.path());
                }
            }
        }
        total
    }

    fn deep(root: &Path, levels: usize) {
        let mut path = root.to_path_buf();
        for _ in 0..levels {
            path.push("d");
        }
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("leaf"), vec![1u8; 9000]).unwrap();
    }

    #[test]
    fn should_measure_exactly_what_a_std_walk_measures() {
        let root = tree(&["a/b/c.txt", "a/d.txt", "e/f/g/h.txt", "i.txt"]);
        std::fs::write(root.path().join("a/big.bin"), vec![3u8; 1_000_000]).unwrap();
        std::fs::hard_link(root.path().join("a/big.bin"), root.path().join("e/again.bin")).unwrap();
        std::os::unix::fs::symlink(root.path().join("a"), root.path().join("e/link")).unwrap();
        deep(root.path(), 40);

        let measured = measure_below(root.path());
        assert_eq!(measured.bytes, std_measure(root.path()));
        assert_eq!(measured.unreadable, 0);
    }

    #[test]
    fn should_count_a_directory_it_cannot_open_as_unreadable() {
        let root = tree(&["locked/inside.txt", "open.txt"]);
        let locked = root.path().join("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let measured = measure_below(root.path());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(measured.unreadable, 1);
    }

    #[test]
    fn should_remove_a_tree_and_count_what_it_freed() {
        let root = tree(&["artifact/a/b/c.txt", "artifact/d.txt", "keep.txt"]);
        let artifact = root.path().join("artifact");
        std::fs::write(artifact.join("a/big.bin"), vec![3u8; 500_000]).unwrap();
        deep(&artifact, 30);
        let expected = std_measure(&artifact) + {
            use std::os::unix::fs::MetadataExt;
            std::fs::symlink_metadata(&artifact).unwrap().blocks() * 512
        };

        let removed = remove_dir(&artifact, false);
        assert!(removed.error.is_none(), "{:?}", removed.error);
        assert!(!artifact.exists());
        assert!(root.path().join("keep.txt").exists());
        assert_eq!(removed.freed, expected);
    }

    #[test]
    fn should_remove_deeper_than_the_fanout_bound() {
        let root = tree(&["keep.txt"]);
        let artifact = root.path().join("artifact");
        deep(&artifact, 300);
        let removed = remove_dir(&artifact, false);
        assert!(removed.error.is_none(), "{:?}", removed.error);
        assert!(!artifact.exists());
    }

    #[test]
    fn should_unlink_a_symlink_and_leave_its_target() {
        let root = tree(&["target/precious.txt", "artifact/x.txt"]);
        std::os::unix::fs::symlink(root.path().join("target"), root.path().join("artifact/link")).unwrap();
        let removed = remove_dir(&root.path().join("artifact"), false);
        assert!(removed.error.is_none(), "{:?}", removed.error);
        assert!(root.path().join("target/precious.txt").exists());
    }

    #[test]
    fn should_leave_a_read_only_directory_without_repair_and_take_everything_else() {
        let root = tree(&["artifact/held/file.txt", "artifact/free/file.txt"]);
        let held = root.path().join("artifact/held");
        std::fs::set_permissions(&held, std::fs::Permissions::from_mode(0o555)).unwrap();
        let removed = remove_dir(&root.path().join("artifact"), false);
        let survived = held.join("file.txt").exists();
        std::fs::set_permissions(&held, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(removed.error.is_some());
        assert!(survived);
        assert!(!root.path().join("artifact/free").exists());
    }

    #[test]
    fn should_repair_a_read_only_directory_when_asked() {
        let root = tree(&["artifact/held/deeper/file.txt", "artifact/locked/file.txt"]);
        let artifact = root.path().join("artifact");
        std::fs::set_permissions(artifact.join("held/deeper"), std::fs::Permissions::from_mode(0o555)).unwrap();
        std::fs::set_permissions(artifact.join("held"), std::fs::Permissions::from_mode(0o555)).unwrap();
        std::fs::set_permissions(artifact.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let removed = remove_dir(&artifact, true);
        assert!(removed.error.is_none(), "{:?}", removed.error);
        assert!(!artifact.exists());
    }

    #[test]
    fn should_never_repair_the_artifacts_parent() {
        let root = tree(&["parent/artifact/file.txt"]);
        let parent = root.path().join("parent");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
        let removed = remove_dir(&parent.join("artifact"), true);
        let mode = std::fs::metadata(&parent).unwrap().permissions().mode();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(removed.error.is_some());
        assert_eq!(mode & 0o777, 0o555);
    }

    #[test]
    fn should_refuse_a_path_that_is_not_a_directory() {
        let root = tree(&["file.txt"]);
        let removed = remove_dir(&root.path().join("file.txt"), false);
        assert!(removed.error.is_some());
        assert!(root.path().join("file.txt").exists());
    }
}
