//! The system calls under [`super`]: opening a directory relative to its parent, and listing it.
//!
//! Every directory is opened with `openat` against a descriptor the walk already holds, so the
//! kernel resolves one path component rather than the whole path from `/` again. On Apple
//! platforms a directory is listed with `getattrlistbulk(2)`, which returns every entry's name,
//! type, allocated size, link count and inode in one call per buffer-full; everywhere else, and
//! on an Apple filesystem that does not support it, it is `readdir` and an `fstatat` against the
//! directory descriptor per entry.
//!
//! Nothing here follows a symlink: directories are opened `O_NOFOLLOW`, and entries are typed by
//! what they are rather than what they point at.

use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// What one directory entry is, never resolved through a symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir,
    File,
    Symlink,
    Other,
}

/// One directory entry and the metadata the walks need from it.
#[derive(Debug)]
pub(crate) struct Entry {
    pub(crate) name: CString,
    pub(crate) kind: Kind,
    /// Allocated bytes, the same quantity as `st_blocks * 512`.
    pub(crate) bytes: u64,
    pub(crate) nlink: u64,
    pub(crate) dev: u64,
    pub(crate) ino: u64,
    /// BSD file flags; always zero where the platform has none.
    pub(crate) flags: u32,
    /// Whether another filesystem is mounted on this directory.
    pub(crate) mount_point: bool,
}

/// A directory's entries, and how many could not be read.
#[derive(Debug, Default)]
pub(crate) struct Listing {
    pub(crate) entries: Vec<Entry>,
    pub(crate) unreadable: usize,
}

/// The `stat` of an open descriptor or of a name relative to one.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Stat {
    pub(crate) kind: Kind,
    pub(crate) bytes: u64,
    pub(crate) mode: u32,
    pub(crate) nlink: u64,
    pub(crate) dev: u64,
    pub(crate) ino: u64,
    pub(crate) flags: u32,
}

/// The BSD user flags that stop an unlink regardless of mode.
#[cfg(target_vendor = "apple")]
pub(crate) const USER_IMMUTABLE: u32 = 0x0000_0002 | 0x0000_0004;

fn check(result: libc::c_int) -> std::io::Result<libc::c_int> {
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Opens a directory by path, without following a symlink in its last component.
pub(crate) fn open_dir(path: &std::path::Path) -> std::io::Result<OwnedFd> {
    use std::os::unix::ffi::OsStrExt;
    let raw = CString::new(path.as_os_str().as_bytes())?;
    open_dir_at(libc::AT_FDCWD, &raw)
}

/// Opens `name` relative to `parent`, refusing anything that is not a directory, including a
/// symlink to one.
pub(crate) fn open_dir_at(parent: RawFd, name: &CStr) -> std::io::Result<OwnedFd> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `name` is NUL-terminated and outlives the call; a non-negative return is a fresh
    // descriptor that nothing else owns.
    let fd = check(unsafe { libc::openat(parent, name.as_ptr(), flags) })?;
    // SAFETY: `fd` was just returned by `openat` and is owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn kind_of(mode: libc::mode_t) -> Kind {
    match mode & libc::S_IFMT {
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFREG => Kind::File,
        libc::S_IFLNK => Kind::Symlink,
        _ => Kind::Other,
    }
}

#[cfg(target_vendor = "apple")]
fn flags_of(stat: &libc::stat) -> u32 {
    stat.st_flags
}

#[cfg(not(target_vendor = "apple"))]
fn flags_of(_stat: &libc::stat) -> u32 {
    0
}

// `allow` rather than `expect`: which of these fire depends on the target's `stat` widths.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::useless_conversion,
    clippy::unnecessary_cast,
    reason = "stat field widths differ across unix targets; every value here is non-negative"
)]
fn stat_of(stat: &libc::stat) -> Stat {
    Stat {
        kind: kind_of(stat.st_mode),
        bytes: (stat.st_blocks as u64).saturating_mul(512),
        mode: u32::from(stat.st_mode),
        nlink: stat.st_nlink as u64,
        dev: stat.st_dev as u64,
        ino: stat.st_ino as u64,
        flags: flags_of(stat),
    }
}

/// `fstatat` with `AT_SYMLINK_NOFOLLOW`.
pub(crate) fn stat_at(parent: RawFd, name: &CStr) -> std::io::Result<Stat> {
    // SAFETY: `stat` is plain data that `fstatat` fills in completely on success.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is NUL-terminated and `stat` is a valid out-pointer for the call.
    check(unsafe { libc::fstatat(parent, name.as_ptr(), &raw mut stat, libc::AT_SYMLINK_NOFOLLOW) })?;
    Ok(stat_of(&stat))
}

/// `fstat` of an open descriptor.
pub(crate) fn stat_fd(fd: RawFd) -> std::io::Result<Stat> {
    // SAFETY: as in `stat_at`.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: the caller holds `fd` open for the duration of the call; `stat` is a valid
    // out-pointer.
    check(unsafe { libc::fstat(fd, &raw mut stat) })?;
    Ok(stat_of(&stat))
}

/// `unlinkat`, of a file (`dir == false`) or an empty directory.
pub(crate) fn unlink_at(parent: RawFd, name: &CStr, dir: bool) -> std::io::Result<()> {
    let flags = if dir { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: `name` is NUL-terminated and outlives the call.
    check(unsafe { libc::unlinkat(parent, name.as_ptr(), flags) }).map(drop)
}

/// Adds the owner's bits in `wanted` to an open directory's mode.
pub(crate) fn add_mode_fd(fd: RawFd, mode: u32, wanted: u32) {
    if mode & wanted == wanted {
        return;
    }
    // SAFETY: `fd` is open for the duration of the call. Failure is deliberately ignored: a
    // repair that could not be applied is not an error, and the retry decides.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a permission mode fits mode_t, whose width differs across unix targets"
    )]
    unsafe {
        libc::fchmod(fd, ((mode | wanted) & 0o7777) as libc::mode_t);
    }
}

/// Adds the owner's bits in `wanted` to the mode of the directory `name`, without following a
/// symlink where the platform allows it.
pub(crate) fn add_mode_at(parent: RawFd, name: &CStr, mode: u32, wanted: u32) {
    if mode & wanted == wanted {
        return;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a permission mode fits mode_t, whose width differs across unix targets"
    )]
    let target = ((mode | wanted) & 0o7777) as libc::mode_t;
    // `AT_SYMLINK_NOFOLLOW` closes the check-then-chmod race under `--force`, where a name just
    // proven a directory by `stat_at` could be swapped for a symlink before the chmod. Apple
    // honours the flag; Linux honours it only through `fchmodat2` (6.6+) and a glibc that uses it,
    // and otherwise fails with `ENOTSUP`/`EINVAL`. So the flag is tried first and the plain form —
    // the gap `std`'s path-based `set_permissions` always carried — is the fallback, never the
    // response to a real permission error.
    for flags in [libc::AT_SYMLINK_NOFOLLOW, 0] {
        // SAFETY: `name` is NUL-terminated and outlives the call; failure is ignored as above.
        let result = unsafe { libc::fchmodat(parent, name.as_ptr(), target, flags) };
        if result == 0 {
            return;
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::ENOTSUP | libc::EINVAL)) {
            return;
        }
    }
}

/// Clears the BSD user-immutable flags on an open descriptor.
#[cfg(target_vendor = "apple")]
pub(crate) fn clear_immutable_fd(fd: RawFd, flags: u32) {
    if flags & USER_IMMUTABLE == 0 {
        return;
    }
    // SAFETY: the caller holds `fd` open for the duration of the call; failure is ignored as
    // above.
    unsafe {
        libc::fchflags(fd, flags & !USER_IMMUTABLE);
    }
}

#[cfg(not(target_vendor = "apple"))]
pub(crate) fn clear_immutable_fd(_fd: RawFd, _flags: u32) {}

/// Clears the BSD user-immutable flags on a non-directory `name`, opened without following it.
#[cfg(target_vendor = "apple")]
pub(crate) fn clear_immutable_at(parent: RawFd, name: &CStr, flags: u32) {
    if flags & USER_IMMUTABLE == 0 {
        return;
    }
    // `O_NONBLOCK` so a FIFO cannot hold the open; `O_NOFOLLOW` so a symlink is never the
    // thing whose flags change.
    let open = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
    // SAFETY: `name` is NUL-terminated and outlives the call.
    let Ok(fd) = check(unsafe { libc::openat(parent, name.as_ptr(), open) }) else {
        return;
    };
    // SAFETY: `fd` was just returned by `openat` and is owned by nobody else.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    clear_immutable_fd(fd.as_raw_fd(), flags);
}

/// Lists an open directory, skipping `.` and `..`.
pub(crate) fn list(dir: &OwnedFd) -> std::io::Result<Listing> {
    #[cfg(target_vendor = "apple")]
    match bulk::list(dir) {
        Err(error) if bulk::unsupported(&error) => {}
        result => return result,
    }
    list_readdir(dir)
}

/// The portable listing: `readdir` over a duplicate of the descriptor, then `fstatat` per entry.
fn list_readdir(dir: &OwnedFd) -> std::io::Result<Listing> {
    // `fdopendir` takes ownership of the descriptor it is given and `closedir` closes it, so it
    // gets a duplicate and `dir` stays the caller's.
    // SAFETY: `dir` is open for the duration of the call.
    let dup = check(unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) })?;
    // SAFETY: `dup` is a fresh descriptor owned by nobody else; on success the stream owns it.
    let stream = unsafe { libc::fdopendir(dup) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so `dup` is still ours to close.
        unsafe { libc::close(dup) };
        return Err(error);
    }
    // SAFETY: `stream` is a valid stream until the `closedir` below.
    unsafe { libc::rewinddir(stream) };

    let mut listing = Listing::default();
    loop {
        // SAFETY: `stream` is valid; the entry it returns lives until the next `readdir`.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `d_name` is a NUL-terminated array inside the entry `readdir` just returned.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        match stat_at(dir.as_raw_fd(), name) {
            Ok(stat) => listing.entries.push(Entry {
                name: name.to_owned(),
                kind: stat.kind,
                bytes: stat.bytes,
                // A directory's link count is its subdirectory count, never a second name for
                // its bytes; both listings report one so nothing deduplicates on it.
                nlink: if stat.kind == Kind::Dir { 1 } else { stat.nlink },
                dev: stat.dev,
                ino: stat.ino,
                flags: stat.flags,
                mount_point: false,
            }),
            // Removed between the listing and the stat: nothing there to count.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => listing.unreadable += 1,
        }
    }
    // SAFETY: `stream` is valid and closed exactly once, which also closes `dup`.
    unsafe { libc::closedir(stream) };

    // A directory's own descriptor tells which entries have another filesystem mounted on them.
    if let Ok(own) = stat_fd(dir.as_raw_fd()) {
        for entry in &mut listing.entries {
            entry.mount_point = entry.kind == Kind::Dir && entry.dev != own.dev;
        }
    }
    Ok(listing)
}

/// `getattrlistbulk(2)`: a directory's entries with their attributes, many per system call.
#[cfg(target_vendor = "apple")]
mod bulk {
    use std::cell::RefCell;
    use std::ffi::CStr;
    use std::os::fd::{AsRawFd, OwnedFd};

    use super::{Entry, Kind, Listing};

    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    unsafe extern "C" {
        fn getattrlistbulk(
            dirfd: libc::c_int,
            attrlist: *mut AttrList,
            buffer: *mut libc::c_void,
            size: libc::size_t,
            options: u64,
        ) -> libc::c_int;
    }

    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_DEVID: u32 = 0x0000_0002;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
    const ATTR_CMN_FILEID: u32 = 0x0200_0000;
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    const ATTR_DIR_MOUNTSTATUS: u32 = 0x0000_0004;
    const ATTR_DIR_ALLOCSIZE: u32 = 0x0000_0008;
    const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
    const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
    const DIR_MNTSTATUS_MNTPOINT: u32 = 0x0000_0001;

    const VREG: u32 = 1;
    const VDIR: u32 = 2;
    const VLNK: u32 = 5;

    /// Large enough for a few thousand entries per call; reused per thread.
    const BUFFER: usize = 256 * 1024;

    thread_local! {
        static SCRATCH: RefCell<Vec<u8>> = RefCell::new(vec![0; BUFFER]);
    }

    /// Whether the filesystem does not offer the call, so the portable listing should run.
    pub(super) fn unsupported(error: &std::io::Error) -> bool {
        matches!(error.raw_os_error(), Some(libc::ENOTSUP | libc::EINVAL | libc::ENOSYS))
    }

    /// A cursor over one packed record.
    struct Record<'a> {
        bytes: &'a [u8],
        at: usize,
    }

    impl Record<'_> {
        fn u32(&mut self) -> u32 {
            let value = self
                .bytes
                .get(self.at..self.at + 4)
                .and_then(|slice| slice.try_into().ok())
                .map_or(0, u32::from_ne_bytes);
            self.at += 4;
            value
        }

        fn u64(&mut self) -> u64 {
            let value = self
                .bytes
                .get(self.at..self.at + 8)
                .and_then(|slice| slice.try_into().ok())
                .map_or(0, u64::from_ne_bytes);
            self.at += 8;
            value
        }
    }

    pub(super) fn list(dir: &OwnedFd) -> std::io::Result<Listing> {
        SCRATCH.with_borrow_mut(|buffer| list_into(dir, buffer))
    }

    fn list_into(dir: &OwnedFd, buffer: &mut [u8]) -> std::io::Result<Listing> {
        let mut attributes = AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: ATTR_CMN_RETURNED_ATTRS
                | ATTR_CMN_ERROR
                | ATTR_CMN_NAME
                | ATTR_CMN_DEVID
                | ATTR_CMN_OBJTYPE
                | ATTR_CMN_FLAGS
                | ATTR_CMN_FILEID,
            volattr: 0,
            dirattr: ATTR_DIR_MOUNTSTATUS | ATTR_DIR_ALLOCSIZE,
            fileattr: ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE,
            forkattr: 0,
        };

        let mut listing = Listing::default();
        loop {
            // SAFETY: `attributes` and `buffer` are valid for the call and `buffer.len()` is its
            // true size; the kernel writes at most that many bytes.
            let count = unsafe {
                getattrlistbulk(
                    dir.as_raw_fd(),
                    &raw mut attributes,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    0,
                )
            };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                // Only the first call decides between this listing and the portable one; an
                // error part-way through is a directory that could not be fully read.
                if listing.entries.is_empty() && listing.unreadable == 0 {
                    return Err(error);
                }
                listing.unreadable += 1;
                return Ok(listing);
            }
            if count == 0 {
                return Ok(listing);
            }
            let mut offset = 0;
            for _ in 0..count {
                let Some(rest) = buffer.get(offset..) else { break };
                let length = Record { bytes: rest, at: 0 }.u32() as usize;
                if length == 0 {
                    break;
                }
                // Clamp both the record and the cursor to what the call actually filled: a record
                // declaring a length past the buffer must not push `offset` out of it, where a
                // wrapped cursor could re-enter the loop.
                let record = &rest[..length.min(rest.len())];
                match parse(record) {
                    Some(Ok(entry)) => listing.entries.push(entry),
                    Some(Err(())) => listing.unreadable += 1,
                    None => {}
                }
                offset += length.min(rest.len());
            }
        }
    }

    /// One packed record, in the order `getattrlist(2)` documents: length, returned set, error,
    /// then the common, directory and file attributes in bit order — but only those the
    /// returned set names. A file's record carries no directory attributes at all and a
    /// directory's no file attributes, so a fixed layout would read one's name as the other's
    /// size; every field is read only when its bit says it is there.
    ///
    /// `None` for `.`/`..` and a nameless record; `Err` for an entry the kernel could not read.
    fn parse(record: &[u8]) -> Option<Result<Entry, ()>> {
        let mut cursor = Record { bytes: record, at: 4 };
        let common = cursor.u32();
        let _volume = cursor.u32();
        let directory = cursor.u32();
        let file = cursor.u32();
        let _fork = cursor.u32();
        let error = if common & ATTR_CMN_ERROR == 0 { 0 } else { cursor.u32() };

        let mut name = None;
        if common & ATTR_CMN_NAME != 0 {
            let at = cursor.at;
            let offset = i64::from(cursor.u32().cast_signed());
            let length = cursor.u32() as usize;
            let start = usize::try_from(i64::try_from(at).ok()? + offset).ok()?;
            name = record
                .get(start..start.checked_add(length)?)
                .and_then(|raw| CStr::from_bytes_until_nul(raw).ok());
        }
        let name = name?;
        if matches!(name.to_bytes(), b"" | b"." | b"..") {
            return None;
        }
        if error != 0 || common & ATTR_CMN_OBJTYPE == 0 {
            return Some(Err(()));
        }

        let dev = if common & ATTR_CMN_DEVID == 0 { 0 } else { cursor.u32() };
        let objtype = cursor.u32();
        let flags = if common & ATTR_CMN_FLAGS == 0 { 0 } else { cursor.u32() };
        let ino = if common & ATTR_CMN_FILEID == 0 { 0 } else { cursor.u64() };
        let mount_status = if directory & ATTR_DIR_MOUNTSTATUS == 0 {
            0
        } else {
            cursor.u32()
        };
        let dir_bytes = if directory & ATTR_DIR_ALLOCSIZE == 0 {
            0
        } else {
            cursor.u64()
        };
        let nlink = if file & ATTR_FILE_LINKCOUNT == 0 {
            1
        } else {
            cursor.u32()
        };
        let file_bytes = if file & ATTR_FILE_ALLOCSIZE == 0 {
            0
        } else {
            cursor.u64()
        };

        let kind = match objtype {
            VDIR => Kind::Dir,
            VREG => Kind::File,
            VLNK => Kind::Symlink,
            _ => Kind::Other,
        };
        let (bytes, nlink) = if kind == Kind::Dir {
            (dir_bytes, 1)
        } else {
            (file_bytes, u64::from(nlink.max(1)))
        };
        Some(Ok(Entry {
            name: name.to_owned(),
            kind,
            bytes,
            nlink,
            dev: u64::from(dev),
            ino,
            flags,
            mount_point: kind == Kind::Dir && mount_status & DIR_MNTSTATUS_MNTPOINT != 0,
        }))
    }

    #[cfg(test)]
    pub(super) fn list_for_test(dir: &OwnedFd) -> std::io::Result<Listing> {
        list(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tree;

    fn sorted(mut listing: Listing) -> Vec<(String, Kind, u64, u64, u64)> {
        listing.entries.sort_by(|left, right| left.name.cmp(&right.name));
        listing
            .entries
            .into_iter()
            .map(|entry| {
                (
                    entry.name.to_string_lossy().into_owned(),
                    entry.kind,
                    entry.bytes,
                    entry.nlink,
                    entry.ino,
                )
            })
            .collect()
    }

    #[test]
    fn should_list_every_kind_of_entry_without_following_a_symlink() {
        let root = tree(&["file.txt", "dir/inner.txt"]);
        std::fs::write(root.path().join("big.bin"), vec![7u8; 300_000]).unwrap();
        std::os::unix::fs::symlink(root.path().join("dir"), root.path().join("link")).unwrap();
        std::fs::hard_link(root.path().join("big.bin"), root.path().join("hard.bin")).unwrap();

        let fd = open_dir(root.path()).unwrap();
        let listing = sorted(list(&fd).unwrap());
        let names: Vec<_> = listing.iter().map(|(name, kind, ..)| (name.as_str(), *kind)).collect();
        assert_eq!(
            names,
            [
                ("big.bin", Kind::File),
                ("dir", Kind::Dir),
                ("file.txt", Kind::File),
                ("hard.bin", Kind::File),
                ("link", Kind::Symlink),
            ]
        );
        let big = listing.iter().find(|(name, ..)| name == "big.bin").unwrap();
        assert!(big.2 >= 300_000, "allocated bytes {} below the file's length", big.2);
        assert_eq!(big.3, 2, "a hard-linked file reports both links");
    }

    /// The fast listing is only worth having if it reports exactly what `fstatat` does.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn should_agree_with_fstatat_on_every_entry() {
        let root = tree(&["a.txt", "nested/b.txt", "nested/deeper/c.txt"]);
        std::fs::write(root.path().join("sized.bin"), vec![1u8; 123_457]).unwrap();
        std::fs::hard_link(root.path().join("a.txt"), root.path().join("a-again.txt")).unwrap();
        std::os::unix::fs::symlink("nowhere", root.path().join("dangling")).unwrap();

        let fd = open_dir(root.path()).unwrap();
        let fast = sorted(bulk::list_for_test(&fd).unwrap());
        let fd = open_dir(root.path()).unwrap();
        let portable = sorted(list_readdir(&fd).unwrap());
        assert_eq!(fast, portable);
    }

    #[test]
    fn should_refuse_to_open_a_symlink_as_a_directory() {
        let root = tree(&["dir/inner.txt"]);
        std::os::unix::fs::symlink(root.path().join("dir"), root.path().join("link")).unwrap();
        let parent = open_dir(root.path()).unwrap();
        let name = CString::new("link").unwrap();
        assert!(open_dir_at(parent.as_raw_fd(), &name).is_err());
    }
}
