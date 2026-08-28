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
const PROTECTED_PATHS: &[&str] = &[
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
fn paths_equal(left: &Path, right: &Path) -> bool {
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
/// Two things make the compiled-in `C:\…` literals insufficient on Windows, and both fail
/// silently rather than visibly:
///
/// 1. [`Path::canonicalize`] returns the *verbatim* form — `\\?\C:\Windows` — which is never
///    equal to `Path::new("C:\\Windows")`. Every rail runs against the canonicalized path, so
///    the literals as written cannot match anything at all. Resolving them here puts both sides
///    of the comparison in the same form.
/// 2. The literals assume the system drive is `C:`. On a machine that boots from `D:` they
///    protect nothing that exists. The environment knows where Windows actually put itself.
///
/// This is additive: `PROTECTED_PATHS` is append-only and is still checked exactly as written.
#[cfg(windows)]
fn resolved_protected_paths() -> Vec<PathBuf> {
    /// Locations Windows publishes for itself. `SystemDrive` is handled separately because it
    /// arrives without a separator.
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
    for variable in SYSTEM_VARIABLES {
        if let Some(value) = std::env::var_os(variable) {
            push(Path::new(&value));
        }
    }
    if let Some(drive) = std::env::var_os("SystemDrive") {
        // `SystemDrive` is `C:` with no separator, and a bare `C:` is *drive-relative*: it
        // resolves to the current directory on that drive, not to its root. Protecting that by
        // accident would refuse every artifact under the working directory.
        let mut root = drive;
        root.push(std::path::MAIN_SEPARATOR_STR);
        push(Path::new(&root));
    }
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

/// Why a removal was refused or failed.
///
/// A refusal is a rail doing its job and is not an error in the run; a failure is an operation
/// that was allowed but did not succeed. The reporter distinguishes them, and only failures
/// change the exit code (ADR 0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The path is a symlink. Never followed, never deleted through.
    Symlink,
    /// The path is a Windows reparse point that is not a symlink — a directory junction being
    /// the one that matters, since it is the no-privilege way to produce the exact hazard
    /// [`Refusal::Symlink`] exists for.
    ///
    /// Reported apart from a symlink because the other reparse tags are not links at all: a
    /// `OneDrive` Files-On-Demand placeholder is refused here too, and telling that user their
    /// directory "is a symlink" would send them looking for something that is not there.
    ReparsePoint,
    /// The path is on the protected denylist.
    Protected,
    /// The path does not resolve strictly below any scan root.
    EscapesRoot,
    /// The path is on a different filesystem than the root that produced it.
    FilesystemBoundary,
    /// The path stopped existing between the walk and the removal.
    Vanished,
}

impl Refusal {
    /// The stable code emitted in JSON.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Symlink => "symlink",
            Self::ReparsePoint => "reparse_point",
            Self::Protected => "protected_path",
            Self::EscapesRoot => "escapes_root",
            Self::FilesystemBoundary => "filesystem_boundary",
            Self::Vanished => "vanished",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // No "refused:" prefix — the renderers already name the outcome, and repeating it
            // reads as a stutter in both the human line and the JSON detail.
            Self::Symlink => write!(f, "is a symlink"),
            Self::ReparsePoint => write!(
                f,
                "is a reparse point — a junction, or a placeholder for a file kept elsewhere"
            ),
            Self::Protected => write!(f, "on the protected-path denylist"),
            Self::EscapesRoot => write!(f, "resolves outside the scan root"),
            Self::FilesystemBoundary => write!(f, "on a different filesystem than its scan root"),
            Self::Vanished => write!(f, "no longer exists"),
        }
    }
}

/// What happened to one artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It was removed.
    Removed,
    /// It passed every rail and would have been removed, but this was a dry run.
    WouldRemove,
    /// A rail refused it.
    Refused(Refusal),
    /// It was allowed but the removal failed.
    Failed(String),
}

impl Outcome {
    /// Whether this outcome should make the process exit non-zero (ADR 0007's code `1`).
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    /// Whether the artifact is gone, or would be.
    #[must_use]
    pub fn is_reclaimed(&self) -> bool {
        matches!(self, Self::Removed | Self::WouldRemove)
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
    /// candidate paths are compared in. Windows only, where the literals cannot match.
    #[cfg(windows)]
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
            #[cfg(windows)]
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
            // A junction is a symlink for every purpose that matters here: `remove_dir_all`
            // through one that points at a source tree deletes that source tree. Every other
            // reparse tag is refused with it — a OneDrive placeholder or a dedup stub left on
            // disk costs the user gigabytes, and following a redirection we did not expect
            // costs them work they cannot get back.
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
        // The literals above are unreachable on Windows, where a canonical path is verbatim;
        // these are that same list plus this machine's system locations, in that form.
        #[cfg(windows)]
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
    pub fn remove(&self, path: &Path, dry_run: bool) -> Outcome {
        let verified = match self.check(path) {
            Ok(verified) => verified,
            Err(refusal) => return Outcome::Refused(refusal),
        };

        if dry_run {
            return Outcome::WouldRemove;
        }

        let result = if verified.is_dir {
            std::fs::remove_dir_all(&verified.canonical)
        } else {
            std::fs::remove_file(&verified.canonical)
        };

        match result {
            Ok(()) => Outcome::Removed,
            // A file vanishing between the walk and the removal is a race, not a failure: the
            // desired state was reached by someone else.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Outcome::Refused(Refusal::Vanished),
            Err(error) => Outcome::Failed(format!("removing `{}`: {error}", verified.canonical.display())),
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
        assert_eq!(guard.remove(&fixture.path().join("target"), false), Outcome::Removed);
        assert_eq!(snapshot(fixture.path()), vec!["Cargo.toml".to_owned()]);
    }

    #[test]
    fn should_remove_a_file_artifact() {
        let fixture = tree(&["package.json", "app.tsbuildinfo"]);
        let guard = guard(fixture.path());
        assert_eq!(
            guard.remove(&fixture.path().join("app.tsbuildinfo"), false),
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
            guard.remove(&fixture.path().join("target"), false),
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
            guard.remove(&elsewhere.path().join("target"), false),
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
            guard.remove(fixture.path(), false),
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
            guard.remove(&fixture.path().join(".GIT"), false),
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
            guard.remove(&fixture.path().join("target"), false),
            Outcome::Refused(Refusal::ReparsePoint)
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

    /// The denylist must fire against the *canonicalized* path, which on Windows is the
    /// verbatim `\\?\C:\…` form that the typed literals can never equal.
    #[test]
    #[cfg(windows)]
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
        assert_eq!(guard.remove(&fixture.path().join("target"), false), Outcome::Removed);
    }

    #[test]
    fn should_refuse_a_vcs_directory_wherever_it_appears() {
        let fixture = tree(&["Cargo.toml", ".git/HEAD", "nested/.git/HEAD"]);
        let guard = guard(fixture.path());
        for vcs in [".git", "nested/.git"] {
            assert_eq!(
                guard.remove(&fixture.path().join(vcs), false),
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
            guard.remove(&fixture.path().join("target"), false),
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

        assert_eq!(guard.remove(&target, true), Outcome::WouldRemove);
        assert_eq!(snapshot(fixture.path()), before, "a dry run must not touch the tree");

        assert_eq!(guard.remove(&target, false), Outcome::Removed);
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
            guard.remove(&fixture.path().join("target"), true),
            Outcome::Refused(Refusal::Symlink)
        );
    }

    #[test]
    fn should_reject_a_root_that_does_not_exist() {
        let error = Guard::new(&[PathBuf::from("/no/such/root")], true).unwrap_err();
        assert!(matches!(error, Error::ReadDir { .. }));
    }

    #[test]
    fn outcomes_classify_failures_and_reclaims() {
        assert!(Outcome::Failed("boom".to_owned()).is_failure());
        assert!(!Outcome::Refused(Refusal::Symlink).is_failure());
        assert!(Outcome::Removed.is_reclaimed());
        assert!(Outcome::WouldRemove.is_reclaimed());
        assert!(!Outcome::Refused(Refusal::Protected).is_reclaimed());
    }

    #[test]
    fn refusal_codes_are_stable() {
        assert_eq!(Refusal::Symlink.code(), "symlink");
        assert_eq!(Refusal::Protected.code(), "protected_path");
        assert_eq!(Refusal::EscapesRoot.code(), "escapes_root");
        assert_eq!(Refusal::FilesystemBoundary.code(), "filesystem_boundary");
        assert_eq!(Refusal::Vanished.code(), "vanished");
    }
}
