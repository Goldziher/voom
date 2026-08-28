//! What a removal concluded: a refusal by a rail, or a failure of the operation itself.
//!
//! Split out from the rails so the vocabulary both reporters match on can be read on its own.
//! A refusal is a rail doing its job; a failure is an operation that was allowed and did not
//! succeed. Only the second changes the exit code (ADR 0007), and that distinction is easier to
//! keep when it is not buried in the middle of the guard.

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
    /// The path is a Windows reparse point that Rust does not report as a symlink.
    ///
    /// Junctions are not this: `FileType::is_symlink` reports them, so a junction is refused as
    /// a [`Refusal::Symlink`] — verified on the Windows CI leg rather than assumed. What lands
    /// here is every *other* reparse tag, none of which is a link: a `OneDrive` Files-On-Demand
    /// placeholder, a dedup stub, a volume mount point. They are refused because following a
    /// redirection voom did not expect can cost work that cannot be got back, and reported
    /// apart from symlinks because telling that user their directory "is a symlink" would send
    /// them looking for something that is not there.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every refusal reaches JSON as a code a hook branches on, so two refusals sharing one
    /// code would make them indistinguishable to a consumer — and a refusal is the tool saying
    /// it declined to delete something, which is exactly what a consumer needs to tell apart.
    #[test]
    fn every_refusal_has_its_own_code_and_its_own_words() {
        let refusals = [
            Refusal::Symlink,
            Refusal::ReparsePoint,
            Refusal::Protected,
            Refusal::EscapesRoot,
            Refusal::FilesystemBoundary,
            Refusal::Vanished,
        ];

        let mut codes = std::collections::HashSet::new();
        let mut words = std::collections::HashSet::new();
        for refusal in refusals {
            assert!(codes.insert(refusal.code()), "duplicate code: {}", refusal.code());
            assert!(words.insert(refusal.to_string()), "duplicate wording: {refusal}");
            assert!(
                !refusal.code().contains(' '),
                "a code is machine-readable: {}",
                refusal.code()
            );
        }
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
