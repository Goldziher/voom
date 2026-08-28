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
//!    become a deletion of that source tree.
//! 2. **A hard-coded protected-path denylist**, matched against the canonical path.
//! 3. **Containment.** Every target must resolve *strictly below* a resolved scan root.
//! 4. **Never cross filesystem boundaries** unless the user opted out.
//! 5. **Isolated failure.** Each artifact is removed independently; one failure is recorded and
//!    reported, and never aborts the run or leaves a state a re-run cannot resolve.
//!
//! `--dry-run` is this same code with the final step withheld — never a parallel
//! implementation — so what it reports is by construction what a real run would do.

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
        #[cfg(not(unix))]
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
            .any(|protected| canonical == Path::new(protected))
        {
            return true;
        }
        if self.home.as_deref() == Some(canonical) {
            return true;
        }
        canonical
            .file_name()
            .is_some_and(|name| VCS_DIRS.iter().any(|vcs| name == std::ffi::OsStr::new(vcs)))
    }

    /// The index of the root this path sits strictly below.
    fn containing_root(&self, canonical: &Path) -> Option<usize> {
        self.roots
            .iter()
            .position(|root| canonical != root.as_path() && canonical.starts_with(root))
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
