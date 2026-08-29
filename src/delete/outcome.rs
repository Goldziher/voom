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

/// Why a removal that passed every rail did not finish.
///
/// Classified from [`std::io::ErrorKind`], never from the raw number. `ENOTEMPTY` is errno 66
/// on macOS, 39 on Linux and `ERROR_DIR_NOT_EMPTY` (145) on Windows — and errno 39 on macOS is
/// something else entirely, so matching on the integer would be a per-platform bug wearing the
/// costume of a portable one. The number is still carried, because it is what a reader will
/// find if they go looking in the OS's own documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureKind {
    /// A directory was not empty at the moment it was unlinked.
    NotEmpty,
    /// Something inside the artifact could not be unlinked.
    Denied,
    /// The filesystem is mounted read-only.
    ReadOnly,
    /// No file descriptor was available to open something with.
    ///
    /// Either this process hit its own limit or the machine hit its table-wide one. Both are a
    /// statement about the moment rather than about the artifact, which is why this is the one
    /// failure besides [`NotEmpty`](Self::NotEmpty) that `--force` waits and retries.
    Exhausted,
    /// Anything else. The OS's own message is the only description.
    Other,
}

impl FailureKind {
    fn of(kind: std::io::ErrorKind) -> Self {
        match kind {
            std::io::ErrorKind::DirectoryNotEmpty => Self::NotEmpty,
            std::io::ErrorKind::PermissionDenied => Self::Denied,
            std::io::ErrorKind::ReadOnlyFilesystem => Self::ReadOnly,
            _ => Self::Other,
        }
    }

    /// Classifies a whole error, so descriptor exhaustion can be recognised.
    ///
    /// **This is the one place voom reads a raw OS number**, and it is worth saying why, since
    /// the rule everywhere else is to key on [`std::io::ErrorKind`] because an errno is not
    /// portable. Here there is no stable kind to key on: `ErrorKind::TooManyOpenFiles` exists
    /// but is unstable (`io_error_too_many_open_files`), so on a stable toolchain EMFILE and
    /// ENFILE both arrive as `Uncategorized` and are indistinguishable from everything else.
    ///
    /// The numbers are therefore named per platform rather than assumed, which is exactly the
    /// mistake keying on 66 for ENOTEMPTY would have been. `should_recognise_descriptor_exhaustion`
    /// checks the mapping on whichever platform the suite runs on, and when the kind stabilises
    /// this collapses back into [`Self::of`].
    fn of_error(error: &std::io::Error) -> Self {
        let kind = Self::of(error.kind());
        if kind != Self::Other {
            return kind;
        }
        if error.raw_os_error().is_some_and(is_exhaustion) {
            return Self::Exhausted;
        }
        kind
    }

    /// The stable code emitted in JSON.
    ///
    /// [`Self::Other`] keeps `removal_failed`, the value schema 1 emitted for every failure, so
    /// a consumer that only ever handled the generic case still sees what it expects.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::NotEmpty => "not_empty",
            Self::Denied => "permission_denied",
            Self::ReadOnly => "read_only_filesystem",
            Self::Exhausted => "descriptors_exhausted",
            Self::Other => "removal_failed",
        }
    }

    /// What would plausibly get the rest of it, when there is anything to suggest.
    #[must_use]
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::NotEmpty => Some("re-run to finish"),
            // Deliberately says "inside", because that is the limit: `--force` never touches the
            // artifact's parent, and a read-only parent is the one shape it cannot fix.
            Self::Denied => Some("--force repairs permissions inside the artifact and retries"),
            Self::Exhausted => Some("--force waits and retries; -j lowers how many run at once"),
            Self::ReadOnly | Self::Other => None,
        }
    }
}

/// Whether a raw OS error number means "no file descriptor was available".
///
/// EMFILE is the process's own limit and ENFILE the machine's table; both are statements about
/// the moment rather than about the artifact, and voom treats them the same.
const fn is_exhaustion(raw: i32) -> bool {
    #[cfg(unix)]
    {
        // EMFILE and ENFILE, identical across Linux and the BSDs including macOS.
        matches!(raw, 24 | 23)
    }
    #[cfg(windows)]
    {
        // ERROR_TOO_MANY_OPEN_FILES.
        raw == 4
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = raw;
        false
    }
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Hedged to exactly the strength the standard library's own documentation uses:
            // `remove_dir_all` "may return DirectoryNotEmpty if the directory is concurrently
            // written into, which typically indicates some contents were removed but not all".
            // It states what the error means and offers the cause as a likelihood; it does not
            // claim to know that a build was running.
            Self::NotEmpty => write!(
                f,
                "a directory was not empty when voom unlinked it, most likely because something \
                 wrote into it during the removal"
            ),
            Self::Denied => write!(f, "permission denied unlinking something inside it"),
            Self::ReadOnly => write!(f, "the filesystem is mounted read-only"),
            Self::Exhausted => write!(
                f,
                "no file descriptors were available — this process or the machine ran out while \
                 the removal was in flight"
            ),
            Self::Other => write!(f, "the removal did not succeed"),
        }
    }
}

/// A removal failure, with the OS's own answer kept beside voom's reading of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalFailure {
    kind: FailureKind,
    os_error: Option<i32>,
    os_message: String,
}

impl RemovalFailure {
    pub(crate) fn from_io(error: &std::io::Error) -> Self {
        Self {
            kind: FailureKind::of_error(error),
            os_error: error.raw_os_error(),
            os_message: error.to_string(),
        }
    }

    /// What kind of failure this was, for a caller that wants to branch rather than read.
    #[must_use]
    pub fn kind(&self) -> FailureKind {
        self.kind
    }

    /// The platform's own error number, when there was one.
    #[must_use]
    pub fn os_error(&self) -> Option<i32> {
        self.os_error
    }

    /// The OS's message, kept whole so nothing it said is lost.
    #[must_use]
    pub fn os_message(&self) -> &str {
        &self.os_message
    }
}

impl std::fmt::Display for RemovalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            // Nothing voom can say about an unrecognised error beats what the OS said.
            FailureKind::Other => write!(f, "{}", self.os_message),
            kind => write!(f, "{kind}"),
        }
    }
}

/// What happened to one artifact.
///
/// `#[non_exhaustive]` because a new outcome is a change this design expects to make — this
/// release adds one — and after 0.1.0 adding a variant to a bare public enum breaks every
/// downstream `match` without a wildcard. [`Refusal`] and [`crate::error::Error`] already carry
/// it; this was the odd one out.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// It was removed.
    Removed,
    /// It passed every rail and would have been removed, but this was a dry run.
    WouldRemove,
    /// A rail refused it.
    Refused(Refusal),
    /// Some of it was removed and some of it was not.
    ///
    /// `remove_dir_all` unlinks a tree's contents before unlinking the tree itself, so a failure
    /// part-way through leaves real space reclaimed and the artifact still on disk. Reporting
    /// that as a plain failure is how a sweep that freed 130 GB came to print a footer saying it
    /// had reclaimed 25.
    ///
    /// `remaining` is measured after the failure and is exact. `freed` is the difference between
    /// that and a size measured before the removal began, so under a writer that is still
    /// working it is an estimate — the direction of the estimate is the only thing that is
    /// guaranteed, and `saturating_sub` is what guarantees it.
    ///
    /// The artifact is **not** reclaimed: it is still there, and it should be retried.
    PartiallyRemoved {
        /// Bytes this removal actually freed.
        freed: u64,
        /// Bytes still on disk, measured after the failure.
        remaining: u64,
        /// Why it stopped.
        failure: RemovalFailure,
    },
    /// It was allowed, and nothing was removed.
    Failed(RemovalFailure),
}

impl Outcome {
    /// Whether this outcome should make the process exit non-zero (ADR 0007's code `1`).
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::PartiallyRemoved { .. })
    }

    /// Whether the artifact is gone, or would be.
    ///
    /// False for [`Outcome::PartiallyRemoved`], which is what watch mode relies on: the tracker
    /// only forgets a path it believes is gone, so a partly-removed artifact stays under the
    /// quiet-period rule and is retried on the next pass rather than being dropped.
    #[must_use]
    pub fn is_reclaimed(&self) -> bool {
        matches!(self, Self::Removed | Self::WouldRemove)
    }
}

#[cfg(test)]
mod tests {

    /// The mapping the errno branch rests on, checked on whichever platform runs the suite.
    ///
    /// A sweep of `/tmp` reported `Too many open files (os error 24)` with no hint and no
    /// retry, because `FailureKind::of` saw `Uncategorized` and answered `Other`. If
    /// `ErrorKind::TooManyOpenFiles` ever stabilises this test keeps passing and the errno
    /// branch can go.
    #[test]
    fn should_recognise_descriptor_exhaustion() {
        for raw in exhaustion_errnos() {
            let failure = RemovalFailure::from_io(&std::io::Error::from_raw_os_error(*raw));
            assert_eq!(
                failure.kind(),
                FailureKind::Exhausted,
                "os error {raw} must be recognised as descriptor exhaustion, not `Other`"
            );
            assert!(
                failure.kind().hint().is_some(),
                "and must suggest something the user can do"
            );
        }
    }

    /// And nothing else is swept up with it: a neighbouring errno must stay `Other`.
    #[test]
    fn should_not_mistake_another_error_for_descriptor_exhaustion() {
        let neighbour = if cfg!(windows) { 5 } else { 25 };
        assert_eq!(
            RemovalFailure::from_io(&std::io::Error::from_raw_os_error(neighbour)).kind(),
            FailureKind::Other
        );
    }

    const fn exhaustion_errnos() -> &'static [i32] {
        if cfg!(windows) { &[4] } else { &[24, 23] }
    }
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
        assert!(
            Outcome::Failed(RemovalFailure::from_io(&std::io::Error::from(
                std::io::ErrorKind::Other
            )))
            .is_failure()
        );
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

    /// Each kind gets its own machine code and its own words, mirroring the discipline
    /// [`Refusal`] follows: a reader who is told "is a symlink" when the problem is a permission
    /// bit goes looking for something that is not there.
    #[test]
    fn every_failure_kind_has_its_own_code_and_its_own_words() {
        let kinds = [
            FailureKind::NotEmpty,
            FailureKind::Denied,
            FailureKind::ReadOnly,
            FailureKind::Other,
        ];
        let mut seen = std::collections::HashSet::new();
        for kind in kinds {
            assert!(seen.insert(kind.code()), "`{}` is used twice", kind.code());
            assert!(!kind.to_string().is_empty(), "{kind:?} has no words");
        }
    }

    /// The classification is by [`std::io::ErrorKind`] and never by the number, because the
    /// number is not portable: `ENOTEMPTY` is 66 on macOS and 39 on Linux, and 39 on macOS means
    /// something else entirely. The number is still carried for anyone reading the OS's docs.
    #[test]
    fn a_failure_is_classified_by_kind_and_still_carries_the_os_number() {
        for (kind, expected, code) in [
            (
                std::io::ErrorKind::DirectoryNotEmpty,
                FailureKind::NotEmpty,
                "not_empty",
            ),
            (
                std::io::ErrorKind::PermissionDenied,
                FailureKind::Denied,
                "permission_denied",
            ),
            (
                std::io::ErrorKind::ReadOnlyFilesystem,
                FailureKind::ReadOnly,
                "read_only_filesystem",
            ),
        ] {
            let failure = RemovalFailure::from_io(&std::io::Error::from(kind));
            assert_eq!(failure.kind(), expected, "{kind:?}");
            assert_eq!(failure.kind().code(), code);
        }

        // A real OS error, so `raw_os_error` is populated rather than synthesised.
        let real = RemovalFailure::from_io(&std::io::Error::from_raw_os_error(if cfg!(target_os = "linux") {
            39
        } else {
            66
        }));
        assert_eq!(real.os_error(), Some(if cfg!(target_os = "linux") { 39 } else { 66 }));
        assert!(!real.os_message().is_empty(), "the OS's own words are kept whole");
    }

    /// Watch mode forgets a path only when it believes the artifact is gone, so a partial
    /// removal must not read as reclaimed — otherwise the tracker drops it and the leftovers are
    /// never retried.
    #[test]
    fn a_partial_removal_is_a_failure_but_is_not_reclaimed() {
        let partial = Outcome::PartiallyRemoved {
            freed: 4096,
            remaining: 512,
            failure: RemovalFailure::from_io(&std::io::Error::from(std::io::ErrorKind::DirectoryNotEmpty)),
        };

        assert!(partial.is_failure(), "it is something that did not finish");
        assert!(!partial.is_reclaimed(), "the artifact is still on disk");
    }
}
