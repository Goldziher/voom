//! The safety rails, and the actual removal.
//!
//! voom deletes by default, with no confirmation prompt, against trees as large as `$HOME`
//! (`adrs/0006-deletion-semantics.md`). The entire safety budget is therefore spent on rails
//! that cannot be clicked through, and every one of them is checked here, immediately before
//! the `remove_dir_all`, against the *canonicalized* path rather than the one that was walked.
//!
//! Marker anchoring (`crate::classify`) is the primary rail and does most of the work. These
//! are the rest:
//!
//! 1. **Never follow symlinks.** A symlinked `target/` pointing at a source tree must not
//!    become a deletion of that source tree. On Windows a directory junction is the same
//!    hazard wearing a different hat, and is refused with them.
//! 2. **A hard-coded protected-path denylist**, matched against the canonical path.
//! 3. **Containment.** Every target must resolve *strictly below* a resolved scan root.
//! 4. **Never cross filesystem boundaries** unless the user opted out.
//! 5. **Isolated failure.** Each artifact is removed independently; one failure is recorded and
//!    reported, and never aborts the run or leaves a state a re-run cannot resolve.
//!
//! `--dry-run` is this same code with the final step withheld — never a parallel
//! implementation — so what it reports is by construction what a real run would do.
//!
//! Two of the rails need platform-specific work to exist at all on Windows, and both look like
//! paranoia until you have met them: canonicalization returns the verbatim (`\\?\C:\…`) form, so
//! a denylist of typed literals can never match it, and paths compare case-insensitively, so a
//! byte-exact comparison is a rail with a hole in it. Each is commented where it is handled.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

mod force;
mod outcome;

pub use force::Removal;
pub use outcome::{FailureKind, Outcome, Refusal, RemovalFailure};

use crate::error::{Error, Result};

/// Paths that are never removed, whatever proves them.
///
/// ~keep This list is **append-only**. Entries may be added; removing or narrowing one requires
/// an ADR amendment explaining why it was safe, never a drive-by edit. See
/// `adrs/0006-deletion-semantics.md` and CONTRIBUTING.
///
/// The check is against the exact canonical path, not a prefix, so `/usr` is protected while
/// `/usr/local/src/project/target` remains removable — protecting subtrees would make the tool
/// useless on the machines it is for.
///
/// The `C:\…` entries are written in the form a human types, which on Windows is *never* the
/// form [`Path::canonicalize`] returns. [`Guard::new`] resolves them, and derives the real
/// system locations from the environment, so that a machine whose system drive is not `C:` is
/// protected too.
pub(crate) const PROTECTED_PATHS: &[&str] = &[
    "/",
    "/Applications",
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/home",
    "/Library",
    "/opt",
    "/private",
    "/proc",
    "/root",
    "/sbin",
    "/srv",
    "/sys",
    "/System",
    "/tmp",
    "/usr",
    "/var",
    "/Users",
    "/Volumes",
    "C:\\",
    "C:\\Program Files",
    "C:\\Program Files (x86)",
    "C:\\Users",
    "C:\\Windows",
];

/// Version-control directories, protected wherever they appear.
const VCS_DIRS: &[&str] = &[".git", ".hg", ".svn"];

/// Whether two path components name the same directory entry.
///
/// Case is significant on unix: `Target` and `target` are two directories there, and folding
/// them together would answer a question about one of them with the other.
#[cfg(not(windows))]
fn names_equal(left: &OsStr, right: &OsStr) -> bool {
    left == right
}

/// Whether two path components name the same directory entry.
///
/// Windows filesystems compare names case-insensitively, so `…\Target` and `…\target` are one
/// directory. Comparing byte-exactly there leaves every name-based rail with a hole in it: a
/// `.GIT`, a `C:\WINDOWS`, or a scan root spelled in another case would each slip past.
#[cfg(windows)]
fn names_equal(left: &OsStr, right: &OsStr) -> bool {
    if left.eq_ignore_ascii_case(right) {
        return true;
    }
    // Windows folds the whole Unicode uppercase table, not only ASCII. This pass runs whenever
    // the ASCII one said no, costing a small allocation per denylist entry per artifact —
    // nothing against the removal it guards.
    left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
}

/// Whether two paths denote the same location, compared component by component so that the
/// platform's name-equality rule applies to each.
pub(crate) fn paths_equal(left: &Path, right: &Path) -> bool {
    #[cfg(not(windows))]
    {
        left == right
    }
    #[cfg(windows)]
    {
        let mut left = left.components();
        let mut right = right.components();
        loop {
            match (left.next(), right.next()) {
                (None, None) => return true,
                (Some(left), Some(right)) if names_equal(left.as_os_str(), right.as_os_str()) => {}
                _ => return false,
            }
        }
    }
}

/// Whether `child` is `ancestor` or sits below it.
///
/// This is [`Path::starts_with`] — component-wise, so `/a/bc` does not start with `/a/b` — with
/// the platform's name-equality rule applied to each component.
fn path_starts_with(child: &Path, ancestor: &Path) -> bool {
    #[cfg(not(windows))]
    {
        child.starts_with(ancestor)
    }
    #[cfg(windows)]
    {
        let mut child = child.components();
        for component in ancestor.components() {
            match child.next() {
                Some(seen) if names_equal(seen.as_os_str(), component.as_os_str()) => {}
                _ => return false,
            }
        }
        true
    }
}

/// The system locations that must never be removed, resolved against this machine.
///
/// Every rail runs against the *canonicalized* candidate, and a protected literal is very often
/// not its own canonical form. Comparing only the literals therefore fails silently rather than
/// visibly — the entry is present, the test that iterates the list passes, and nothing is
/// actually protected:
///
/// - On macOS `/tmp`, `/etc` and `/var` are symlinks into `/private`, and `/home` is an
///   automount. A candidate resolving to `/private/etc` is not equal to `/etc`.
/// - On a usrmerge Linux — Arch, Fedora, Debian since Bullseye — `/bin`, `/sbin` and `/lib`
///   are symlinks into `/usr`. `/usr` is listed, but as an exact match, so it does not cover
///   `/usr/bin`.
/// - On Windows [`Path::canonicalize`] returns the verbatim form `\\?\C:\Windows`, which is
///   never equal to `Path::new("C:\\Windows")`, and the literals additionally assume the system
///   drive is `C:` when the environment knows where Windows actually put itself.
///
/// Resolving the list once at construction puts both sides of every comparison in the same form
/// and lets the denylist follow whatever a given OS's layout resolves to, instead of requiring
/// each platform's private-mount naming to be hand-enumerated.
///
/// This is additive and never subtractive: `PROTECTED_PATHS` is append-only and is still checked
/// exactly as written, so an entry that cannot be canonicalized (it does not exist on this
/// machine) still protects its literal spelling.
pub(crate) fn resolved_protected_paths() -> Vec<PathBuf> {
    /// Locations Windows publishes for itself. `SystemDrive` is handled separately because it
    /// arrives without a separator.
    #[cfg(windows)]
    const SYSTEM_VARIABLES: &[&str] = &[
        "SystemRoot",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
        "USERPROFILE",
    ];

    let mut resolved: Vec<PathBuf> = Vec::new();
    let mut push = |path: &Path| {
        if let Ok(canonical) = path.canonicalize()
            && !resolved.iter().any(|seen| paths_equal(seen, &canonical))
        {
            resolved.push(canonical);
        }
    };

    for protected in PROTECTED_PATHS {
        push(Path::new(protected));
    }
    #[cfg(windows)]
    for variable in SYSTEM_VARIABLES {
        if let Some(value) = std::env::var_os(variable) {
            push(Path::new(&value));
        }
    }
    #[cfg(windows)]
    if let Some(drive) = std::env::var_os("SystemDrive") {
        // `SystemDrive` is `C:` with no separator, and a bare `C:` is *drive-relative*: it
        // resolves to the current directory on that drive, not to its root. Protecting that by
        // accident would refuse every artifact under the working directory.
        let mut root = drive;
        root.push(std::path::MAIN_SEPARATOR_STR);
        push(Path::new(&root));
    }
    #[cfg(windows)]
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        // The profile's parent is this machine's `Users` root, wherever it was put.
        if let Some(parent) = Path::new(&profile).parent() {
            push(parent);
        }
    }
    resolved
}

/// The reparse-point attribute bit, from `winnt.h`.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// Whether this entry redirects somewhere else.
///
/// `FileType::is_symlink` reports the reparse tags Windows marks as name surrogates and is not
/// guaranteed to report the rest. A directory junction is the exact hazard rail 1 exists for —
/// `remove_dir_all` through a junction that points at a source tree deletes that source tree —
/// so the attribute bit is read directly rather than trusted to a tag classification.
#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// The volume-naming prefix of a path: the drive letter, or the UNC server and share.
///
/// `None` for a path that has no prefix, which a canonicalized Windows path always does.
#[cfg(windows)]
fn volume_prefix(path: &Path) -> Option<&OsStr> {
    match path.components().next() {
        Some(std::path::Component::Prefix(prefix)) => Some(prefix.as_os_str()),
        _ => None,
    }
}

/// Whether two paths are named on the same volume, as far as the path itself can say.
#[cfg(windows)]
fn same_volume(left: &Path, right: &Path) -> bool {
    match (volume_prefix(left), volume_prefix(right)) {
        (Some(left), Some(right)) => names_equal(left, right),
        // A path we cannot read a volume from is one we cannot clear, so it does not get cleared.
        _ => false,
    }
}

/// A path that has passed every rail.
///
/// Only [`Guard::check`] can mint one, so it is not possible to reach [`Guard::remove`] with a
/// path that was not verified — the rails are in the type, not only in the call order.
#[derive(Debug)]
pub struct Verified {
    canonical: PathBuf,
    is_dir: bool,
}

impl Verified {
    /// The resolved path that will actually be removed.
    #[must_use]
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }
}

/// Holds the resolved scan roots and enforces the rails against them.
#[derive(Debug)]
pub struct Guard {
    roots: Vec<PathBuf>,
    #[cfg(unix)]
    root_devices: Vec<u64>,
    /// `PROTECTED_PATHS` and this machine's system locations, resolved into the same form the
    /// candidate paths are compared in — which the literals frequently are not, on every
    /// platform. See [`resolved_protected_paths`].
    resolved_protected: Vec<PathBuf>,
    home: Option<PathBuf>,
    one_file_system: bool,
}

impl Guard {
    /// Resolves the scan roots once, so that per-artifact checking is a comparison rather than
    /// a syscall.
    ///
    /// # Errors
    ///
    /// [`Error::ReadDir`] if a root cannot be resolved. A root that does not exist is a usage
    /// error and must stop the run before anything is deleted.
    pub fn new(roots: &[PathBuf], one_file_system: bool) -> Result<Self> {
        let mut resolved = Vec::with_capacity(roots.len());
        #[cfg(unix)]
        let mut devices = Vec::with_capacity(roots.len());

        for root in roots {
            let canonical = root.canonicalize().map_err(|source| Error::ReadDir {
                path: root.clone(),
                source,
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let metadata = canonical.metadata().map_err(|source| Error::ReadDir {
                    path: canonical.clone(),
                    source,
                })?;
                devices.push(metadata.dev());
            }
            resolved.push(canonical);
        }

        Ok(Self {
            roots: resolved,
            #[cfg(unix)]
            root_devices: devices,
            resolved_protected: resolved_protected_paths(),
            home: dirs::home_dir().and_then(|home| home.canonicalize().ok()),
            one_file_system,
        })
    }

    /// The canonicalized scan roots.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Runs every rail. Returns the verified path, or the rail that refused it.
    ///
    /// # Errors
    ///
    /// Never returns an [`Error`]; the failure type is a [`Refusal`], because a rail refusing a
    /// path is the system working rather than an error in the run.
    pub fn check(&self, path: &Path) -> std::result::Result<Verified, Refusal> {
        // 1. Symlinks. `symlink_metadata` does not follow, so this sees the link itself. It
        //    also answers "does this still exist" and "is it a directory" without a second
        //    syscall.
        let metadata = std::fs::symlink_metadata(path).map_err(|_| Refusal::Vanished)?;
        if metadata.file_type().is_symlink() {
            return Err(Refusal::Symlink);
        }
        #[cfg(windows)]
        if is_reparse_point(&metadata) {
            // The backstop for rail 1. Junctions are already caught above — Rust reports them
            // as symlinks — so what reaches here is the reparse tags it does not classify: a
            // mount point, a OneDrive placeholder, a dedup stub. Refused rather than followed,
            // because a redirection voom did not expect costs work that cannot be got back,
            // while a directory left on disk costs only the space it occupies.
            return Err(Refusal::ReparsePoint);
        }

        // 2. Resolve. Everything below is checked against the *resolved* path, because the path
        //    that was walked is not necessarily the path that will be unlinked.
        let canonical = path.canonicalize().map_err(|_| Refusal::Vanished)?;

        if self.is_protected(&canonical) {
            return Err(Refusal::Protected);
        }

        // 3. Containment: strictly below a root. Equality is refused too — voom removes things
        //    *in* the tree it was pointed at, never the tree itself.
        let Some(index) = self.containing_root(&canonical) else {
            return Err(Refusal::EscapesRoot);
        };

        // 4. Filesystem boundary.
        #[cfg(unix)]
        if self.one_file_system {
            use std::os::unix::fs::MetadataExt;
            if self
                .root_devices
                .get(index)
                .is_some_and(|device| *device != metadata.dev())
            {
                return Err(Refusal::FilesystemBoundary);
            }
        }
        #[cfg(windows)]
        if self.one_file_system {
            // The walker's `same_file_system` is the unix `dev()` and does nothing on Windows,
            // so the boundary has to be enforced here or not at all — and a flag documented as
            // a safety control must not be a no-op.
            //
            // Windows offers no *stable* way to ask which volume a path is on:
            // `MetadataExt::volume_serial_number` is unstable (rust-lang/rust#63010) and every
            // alternative is FFI. So the rail is kept by the shape of the path plus rail 1,
            // which between them leave nothing uncovered:
            //
            //  * Another drive letter or UNC share is a different path prefix. Rail 3 has
            //    already refused such a target for not being below the root; comparing prefixes
            //    here keeps this rail responsible for its own promise rather than resting on a
            //    side effect of another one.
            //  * The only way to reach another volume while staying below the root is through a
            //    volume mount point, which is a reparse point, which rail 1 refuses — on
            //    Windows unconditionally, so `--one-file-system=false` is stricter there than
            //    it asks for rather than looser, which is the direction to err in.
            if self.roots.get(index).is_some_and(|root| !same_volume(root, &canonical)) {
                return Err(Refusal::FilesystemBoundary);
            }
        }
        #[cfg(not(any(unix, windows)))]
        let _ = index;

        Ok(Verified {
            canonical,
            is_dir: metadata.file_type().is_dir(),
        })
    }

    /// Whether the canonical path is one voom never removes.
    fn is_protected(&self, canonical: &Path) -> bool {
        if canonical.parent().is_none() {
            return true;
        }
        if PROTECTED_PATHS
            .iter()
            .any(|protected| paths_equal(canonical, Path::new(protected)))
        {
            return true;
        }
        // The same list, canonicalized against this machine — which is the form every rail
        // actually compares, and which the literals above frequently are not. See
        // `resolved_protected_paths`.
        if self
            .resolved_protected
            .iter()
            .any(|protected| paths_equal(canonical, protected))
        {
            return true;
        }
        if self.home.as_deref().is_some_and(|home| paths_equal(canonical, home)) {
            return true;
        }
        canonical
            .file_name()
            .is_some_and(|name| VCS_DIRS.iter().any(|vcs| names_equal(name, OsStr::new(vcs))))
    }

    /// The index of the root this path sits strictly below.
    fn containing_root(&self, canonical: &Path) -> Option<usize> {
        self.roots
            .iter()
            .position(|root| !paths_equal(canonical, root) && path_starts_with(canonical, root))
    }

    /// Checks the rails and then removes, or withholds the removal for a dry run.
    ///
    /// This is the only path to a deletion. `dry_run` gates the final step and nothing else, so
    /// a dry run exercises every rail a real run does and cannot disagree with one.
    #[must_use]
    pub fn remove(&self, path: &Path, bytes: u64, removal: Removal) -> Outcome {
        let verified = match self.check(path) {
            // Every rail runs first and unconditionally. `removal` is not read until after this
            // returns, so no setting on it can reach a path a rail refused.
            Ok(verified) => verified,
            Err(refusal) => return Outcome::Refused(refusal),
        };

        if removal.dry_run {
            return Outcome::WouldRemove;
        }

        match force::remove(&verified, removal) {
            Ok(()) => Outcome::Removed,
            // A file vanishing between the walk and the removal is a race, not a failure: the
            // desired state was reached by someone else.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Outcome::Refused(Refusal::Vanished),
            Err(error) => Self::failed(&verified, bytes, &error),
        }
    }

    /// Tells a removal that freed nothing from one that freed most of an artifact.
    ///
    /// `remove_dir_all` unlinks a tree's contents before the tree itself, so a failure part-way
    /// through is the common case rather than the exotic one, and reporting it as though nothing
    /// happened understates a sweep by however much it actually freed.
    ///
    /// The residue is measured here and nowhere else, so a run in which nothing fails pays
    /// nothing for it — ADR 0005's "measure each artifact once" is about the artifact, and this
    /// is a measurement of what is left of it.
    ///
    /// The path is deliberately absent from the message: every reporter already names it on the
    /// same line, and repeating it there put the longest string on the line twice.
    fn failed(verified: &Verified, bytes: u64, error: &std::io::Error) -> Outcome {
        let failure = RemovalFailure::from_io(error);
        if !verified.is_dir {
            return Outcome::Failed(failure);
        }
        let remaining = crate::size::measure(&verified.canonical);
        // `bytes` was measured before the removal began, so under a writer that is still working
        // the two are not measurements of the same tree. Saturating is what keeps the estimate
        // on the honest side of the truth rather than reporting a negative reclaim.
        //
        // One direction stays an estimate: `size::measure` counts a subtree it cannot list as
        // zero, so if something outside voom tightens a permission between the two measurements,
        // `remaining` undercounts and `freed` overstates. Nothing voom does can cause it —
        // `--force` only ever widens — and it costs a number in the report, never a deletion.
        let freed = bytes.saturating_sub(remaining);
        if freed == 0 {
            return Outcome::Failed(failure);
        }
        Outcome::PartiallyRemoved {
            freed,
            remaining,
            failure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{snapshot, tree};

    fn guard(root: &Path) -> Guard {
        Guard::new(&[root.to_path_buf()], true).expect("the root resolves")
    }

    #[test]
    fn should_remove_a_verified_directory() {
        let fixture = tree(&["Cargo.toml", "target/debug/build.o"]);
        let guard = guard(fixture.path());
        assert_eq!(
            guard.remove(&fixture.path().join("target"), 0, Removal::new()),
            Outcome::Removed
        );
        assert_eq!(snapshot(fixture.path()), vec!["Cargo.toml".to_owned()]);
    }

    #[test]
    fn should_remove_a_file_artifact() {
        let fixture = tree(&["package.json", "app.tsbuildinfo"]);
        let guard = guard(fixture.path());
        assert_eq!(
            guard.remove(&fixture.path().join("app.tsbuildinfo"), 0, Removal::new()),
            Outcome::Removed
        );
        assert_eq!(snapshot(fixture.path()), vec!["package.json".to_owned()]);
    }

    /// The rail that stops a symlinked `target/` from becoming a deletion of whatever it
    /// points at.
    #[test]
    #[cfg(unix)]
    fn should_refuse_a_symlink_and_leave_its_target_intact() {
        let fixture = tree(&["Cargo.toml", "source/precious.rs"]);
        std::os::unix::fs::symlink(fixture.path().join("source"), fixture.path().join("target")).expect("a symlink");

        let guard = guard(fixture.path());
        assert_eq!(
            guard.remove(&fixture.path().join("target"), 0, Removal::new()),
            Outcome::Refused(Refusal::Symlink)
        );
        assert!(
            fixture.path().join("source/precious.rs").exists(),
            "the target survives"
        );
    }

    #[test]
    fn should_refuse_a_path_outside_every_scan_root() {
        let scanned = tree(&["Cargo.toml", "target/"]);
        let elsewhere = tree(&["Cargo.toml", "target/"]);
        let guard = guard(scanned.path());
        assert_eq!(
            guard.remove(&elsewhere.path().join("target"), 0, Removal::new()),
            Outcome::Refused(Refusal::EscapesRoot)
        );
        assert!(elsewhere.path().join("target").exists());
    }

    /// Containment is *strictly* below. voom removes things in the tree it was pointed at,
    /// never the tree itself.
    #[test]
    fn should_refuse_the_scan_root_itself() {
        let fixture = tree(&["Cargo.toml"]);
        let guard = guard(fixture.path());
        assert_eq!(
            guard.remove(fixture.path(), 0, Removal::new()),
            Outcome::Refused(Refusal::EscapesRoot)
        );
        assert!(fixture.path().exists());
    }

    /// Every entry on the denylist is refused. This iterates the list itself, so an entry added
    /// later is covered without anybody remembering to add a test.
    #[test]
    fn should_refuse_every_protected_path() {
        let fixture = tree(&["Cargo.toml"]);
        let guard = guard(fixture.path());
        for protected in PROTECTED_PATHS {
            let path = Path::new(protected);
            if !path.exists() {
                continue;
            }
            match guard.check(path) {
                // A protected path that is also a symlink (`/var` on macOS) is refused one
                // rail earlier, which is just as good.
                Err(Refusal::Protected | Refusal::Symlink) => {}
                other => panic!("`{protected}` was not refused: {other:?}"),
            }
        }
    }

    /// Path comparison is case-*sensitive* off Windows, and must stay that way: `Target` and
    /// `target` are two directories on a case-sensitive filesystem, and answering a question
    /// about one of them with the other is how a rail deletes the wrong thing. Pinned on the
    /// comparison itself rather than on a removal, so it never reads as an endorsement of
    /// deleting a differently-cased `.git`.
    #[test]
    #[cfg(not(windows))]
    fn should_compare_paths_case_sensitively_off_windows() {
        assert!(!names_equal(OsStr::new(".GIT"), OsStr::new(".git")));
        assert!(!paths_equal(Path::new("/Users"), Path::new("/users")));
        assert!(!path_starts_with(Path::new("/a/B/c"), Path::new("/a/b")));
        assert!(paths_equal(Path::new("/a/b"), Path::new("/a/b")));
        assert!(path_starts_with(Path::new("/a/b/c"), Path::new("/a/b")));
    }

    /// Windows compares names case-insensitively, so the rails must too — otherwise a
    /// differently-cased spelling of a protected path or of a scan root walks straight past
    /// them. `/a/bc` still does not start with `/a/b`: the comparison is component-wise.
    #[test]
    #[cfg(windows)]
    fn should_compare_paths_case_insensitively_on_windows() {
        assert!(names_equal(OsStr::new(".GIT"), OsStr::new(".git")));
        assert!(paths_equal(Path::new(r"C:\Windows"), Path::new(r"c:\windows")));
        assert!(path_starts_with(
            Path::new(r"C:\Users\X\Target"),
            Path::new(r"c:\users\x")
        ));
        assert!(!path_starts_with(Path::new(r"C:\a\bc"), Path::new(r"C:\a\b")));
        assert!(!paths_equal(
            Path::new(r"C:\Windows"),
            Path::new(r"C:\Windows\System32")
        ));
    }

    /// A `.GIT` on a Windows filesystem *is* `.git`, so it is refused there.
    #[test]
    #[cfg(windows)]
    fn should_refuse_a_differently_cased_vcs_directory_on_windows() {
        let fixture = tree(&["Cargo.toml", ".GIT/HEAD"]);
        let guard = guard(fixture.path());
        assert_eq!(
            guard.remove(&fixture.path().join(".GIT"), 0, Removal::new()),
            Outcome::Refused(Refusal::Protected)
        );
        assert!(fixture.path().join(".GIT/HEAD").exists());
    }

    /// The Windows form of the symlink rail. A junction needs no privilege to create, which is
    /// exactly why it is the likely shape of the "a linked `target/` deletes your source tree"
    /// accident there.
    #[test]
    #[cfg(windows)]
    fn should_refuse_a_junction_and_leave_its_target_intact() {
        let fixture = tree(&["Cargo.toml", "source/precious.rs"]);
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(fixture.path().join("target"))
            .arg(fixture.path().join("source"))
            .status()
            .expect("mklink runs");
        assert!(status.success(), "the fixture junction must be created");

        let guard = guard(fixture.path());
        assert_eq!(
            guard.remove(&fixture.path().join("target"), 0, Removal::new()),
            // Refused as a symlink, not as a bare reparse point: Rust's `is_symlink` does
            // report junctions on Windows, so rail 1 catches this before the reparse backstop.
            // The variant matters less than the refusal, but pin it so a change to either is
            // noticed rather than absorbed.
            Outcome::Refused(Refusal::Symlink)
        );
        assert!(
            fixture.path().join("source/precious.rs").exists(),
            "the junction's target survives"
        );
    }

    /// The system locations come from the environment, not from a hardcoded `C:`, so a machine
    /// that boots from another drive is protected too.
    #[test]
    #[cfg(windows)]
    fn should_resolve_the_windows_system_locations_from_the_environment() {
        let resolved = resolved_protected_paths();
        let system_root = std::env::var_os("SystemRoot").expect("Windows sets SystemRoot");
        let canonical = Path::new(&system_root).canonicalize().expect("SystemRoot resolves");
        assert!(
            resolved.iter().any(|path| paths_equal(path, &canonical)),
            "the real Windows directory must be protected: {resolved:?}"
        );
    }

    /// Every protected literal, in the form the rails actually compare against.
    ///
    /// The rails run on canonicalized paths, and a protected literal is very often not its own
    /// canonical form: on macOS `/tmp`, `/etc` and `/var` are symlinks into `/private`, on a
    /// usrmerge Linux `/bin` and `/sbin` are symlinks into `/usr`, and on Windows every path
    /// canonicalizes to the verbatim `\\?\C:\…` spelling. A denylist checked only as written
    /// therefore passes its own test while protecting nothing that a real candidate resolves to.
    #[test]
    fn should_refuse_every_resolved_protected_path() {
        let fixture = tree(&["Cargo.toml"]);
        let guard = guard(fixture.path());
        for protected in &guard.resolved_protected {
            match guard.check(protected) {
                Err(Refusal::Protected | Refusal::Symlink) => {}
                other => panic!("`{}` was not refused: {other:?}", protected.display()),
            }
        }
    }

    /// What the Windows boundary rail can and cannot see from a path: another drive or share is
    /// a crossing, a difference in case is not, and a path with no volume in it is not cleared.
    #[test]
    #[cfg(windows)]
    fn should_read_the_volume_from_a_windows_path() {
        assert!(same_volume(Path::new(r"\\?\C:\a"), Path::new(r"\\?\c:\a\b")));
        assert!(!same_volume(Path::new(r"\\?\C:\a"), Path::new(r"\\?\D:\a\b")));
        assert!(!same_volume(
            Path::new(r"\\?\UNC\server\one"),
            Path::new(r"\\?\UNC\server\two")
        ));
        assert!(!same_volume(Path::new("relative"), Path::new(r"\\?\C:\a")));
    }

    /// The boundary rail must refuse crossings, not everything: with `--one-file-system` on —
    /// the default — an artifact on the root's own volume is still removed. This is the
    /// regression test for the Windows implementation of rail 4 being too strict.
    #[test]
    fn should_remove_an_artifact_on_the_same_filesystem_as_its_root() {
        let fixture = tree(&["Cargo.toml", "target/debug/build.o"]);
        let guard = Guard::new(&[fixture.path().to_path_buf()], true).expect("the root resolves");
        assert_eq!(
            guard.remove(&fixture.path().join("target"), 0, Removal::new()),
            Outcome::Removed
        );
    }

    /// The denylist is append-only (ADR 0006, CONTRIBUTING). `should_refuse_every_protected_path`
    /// iterates whatever the list happens to contain, so it passes just as happily after someone
    /// removes an entry — which is the edit the rule exists to stop. This names the entries, so
    /// removing one fails here and the failure says which rule was broken.
    ///
    /// Adding an entry does not fail this test. That is the asymmetry the rule asks for.
    #[test]
    fn the_protected_denylist_is_append_only() {
        const REQUIRED: &[&str] = &[
            "/",
            "/Applications",
            "/bin",
            "/boot",
            "/dev",
            "/etc",
            "/home",
            "/Library",
            "/opt",
            "/private",
            "/proc",
            "/root",
            "/sbin",
            "/srv",
            "/sys",
            "/System",
            "/tmp",
            "/usr",
            "/var",
            "/Users",
            "/Volumes",
            "C:\\",
            "C:\\Program Files",
            "C:\\Program Files (x86)",
            "C:\\Users",
            "C:\\Windows",
        ];

        for required in REQUIRED {
            assert!(
                PROTECTED_PATHS.contains(required),
                "`{required}` was removed from PROTECTED_PATHS. The list is append-only: \
                 removing or narrowing an entry needs an ADR 0006 amendment saying why it was \
                 safe, never a drive-by edit."
            );
        }

        for vcs in [".git", ".hg", ".svn"] {
            assert!(VCS_DIRS.contains(&vcs), "`{vcs}` was removed from VCS_DIRS");
        }
    }

    #[test]
    fn should_refuse_a_vcs_directory_wherever_it_appears() {
        let fixture = tree(&["Cargo.toml", ".git/HEAD", "nested/.git/HEAD"]);
        let guard = guard(fixture.path());
        for vcs in [".git", "nested/.git"] {
            assert_eq!(
                guard.remove(&fixture.path().join(vcs), 0, Removal::new()),
                Outcome::Refused(Refusal::Protected),
                "`{vcs}` must be refused"
            );
        }
        assert!(fixture.path().join(".git/HEAD").exists());
    }

    #[test]
    fn should_refuse_the_home_directory_itself() {
        let Some(home) = dirs::home_dir() else { return };
        let fixture = tree(&["Cargo.toml"]);
        let guard = guard(fixture.path());
        assert_eq!(guard.check(&home).unwrap_err(), Refusal::Protected);
    }

    #[test]
    fn should_refuse_a_path_that_vanished_between_the_walk_and_the_removal() {
        let fixture = tree(&["Cargo.toml"]);
        let guard = guard(fixture.path());
        assert_eq!(
            guard.remove(&fixture.path().join("target"), 0, Removal::new()),
            Outcome::Refused(Refusal::Vanished)
        );
    }

    /// `--dry-run` is the same pipeline with the final step withheld. This asserts both halves
    /// of that claim: the tree is byte-identical afterwards, and what the dry run said it would
    /// remove is exactly what a real run then removes.
    #[test]
    fn should_leave_the_tree_untouched_and_agree_with_a_real_run() {
        let fixture = tree(&["Cargo.toml", "target/debug/build.o", "src/main.rs"]);
        let guard = guard(fixture.path());
        let before = snapshot(fixture.path());
        let target = fixture.path().join("target");

        assert_eq!(guard.remove(&target, 0, Removal::dry()), Outcome::WouldRemove);
        assert_eq!(snapshot(fixture.path()), before, "a dry run must not touch the tree");

        assert_eq!(guard.remove(&target, 0, Removal::new()), Outcome::Removed);
        assert_eq!(
            snapshot(fixture.path()),
            vec!["Cargo.toml".to_owned(), "src/".to_owned(), "src/main.rs".to_owned()]
        );
    }

    /// A dry run must exercise every rail, not skip to the answer — otherwise its report is a
    /// simulation rather than a preview.
    #[test]
    #[cfg(unix)]
    fn should_apply_every_rail_during_a_dry_run() {
        let fixture = tree(&["Cargo.toml", "source/precious.rs"]);
        std::os::unix::fs::symlink(fixture.path().join("source"), fixture.path().join("target")).expect("a symlink");
        let guard = guard(fixture.path());
        assert_eq!(
            guard.remove(&fixture.path().join("target"), 0, Removal::dry()),
            Outcome::Refused(Refusal::Symlink)
        );
    }

    #[test]
    fn should_reject_a_root_that_does_not_exist() {
        let error = Guard::new(&[PathBuf::from("/no/such/root")], true).unwrap_err();
        assert!(matches!(error, Error::ReadDir { .. }));
    }
}
