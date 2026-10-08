//! Bazel output-base housekeeping — orphaned and stale per-workspace scratch that nothing else
//! reclaims.
//!
//! Every distinct workspace path that has run Bazel gets its own output base, and nothing
//! removes it when the workspace goes away: not `git worktree prune` (the output base is not
//! under `.git`), and not the [`caches`](crate::caches) table (that proves one fixed location,
//! not one of however many a user has ever pointed Bazel at). See
//! `adrs/0014-bazel-output-base-housekeeping.md`.
//!
//! Bazel already answers the question that matters — is the workspace this output base belongs
//! to still there — by writing `DO_NOT_BUILD_HERE` into the output base's root, containing the
//! absolute path of the owner. [`prune`] reads it and removes an output base when its owner is
//! gone, when the owner is a git worktree whose administration is gone (`git worktree remove`
//! run from elsewhere, or a `.git` file pointing nowhere), or when nothing has built there for
//! longer than [`BazelPruneOptions::max_age`]. A base with a live server is never touched unless
//! `clear_all` asks for the whole lot, in which case the server is stopped first. Install bases
//! no surviving output base points at, and shared-cache entries older than the age, go the same
//! way. Rendering lives in [`report`].

mod report;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rayon::prelude::*;

pub use report::{document, render_human, render_json};

use crate::delete::{Guard, Outcome, Removal};
use crate::error::Result;
use crate::size::measure_fully;

/// The JSON shape's version, versioned separately from the sweep's because it is a separate
/// document sharing only the envelope.
pub const SCHEMA_VERSION: u32 = 1;

/// How long an output base may go unbuilt-in before it is stale, when nobody says otherwise.
///
/// A week: long enough that a worktree parked over a weekend keeps its warm analysis cache,
/// short enough that the fortnight-old ticket branches a monorepo accumulates do not each hold
/// on to several gigabytes.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The file Bazel writes into an output base's root, naming the workspace that owns it.
///
/// Read from the output base's own root first, and failing that from
/// `<output base>/execroot/DO_NOT_BUILD_HERE` — where a shared, sequentially-reused
/// output-user-root (one Docker-wrapping build tool measured for this ADR points every
/// workspace's build at the same output base in turn) puts it *instead of* the root location.
const OWNER_MARKER: &str = "DO_NOT_BUILD_HERE";

/// Entries Bazel touches on every invocation; the newest modification time among them (and the
/// output base's own) is when the base was last built in.
const ACTIVITY_MARKERS: &[&str] = &[
    "command.log",
    "lock",
    "action_cache",
    "execroot",
    "external",
    "server",
    "java.log",
];

/// A control file voom reads (`DO_NOT_BUILD_HERE`, a `.git` file, a pid) is a path or a number,
/// never large. Anything bigger is not one of those, and a FIFO or an enormous file at that path
/// must not block or exhaust the process, so it is refused rather than read.
const CONTROL_FILE_MAX_BYTES: u64 = 4096;

/// Overrides [`conventional_roots`].
pub const ROOTS_ENV: &str = "VOOM_BAZEL_ROOTS";

/// Where Bazel records a running server's process id, inside the output base.
const SERVER_PID_FILE: &str = "server/server.pid.txt";

/// How long `clear_all` waits for a stopped server to exit.
const SERVER_STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// What to search, and how.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "these mirror command-line flags, which are bools by nature"
)]
pub struct BazelPruneOptions {
    /// Output-user-roots to search. Each candidate is one of this root's immediate
    /// subdirectories — an output base is never nested any deeper than that.
    pub roots: Vec<PathBuf>,
    /// Withhold the removal and report what would happen.
    pub dry_run: bool,
    /// Retry a failed removal, repairing permissions inside the artifact first.
    pub force: bool,
    /// Whether removal stays on the root's filesystem.
    pub one_file_system: bool,
    /// How long an output base, or a shared-cache entry, may sit unused before it is stale.
    pub max_age: Duration,
    /// Remove everything under a `_bazel_<user>` root — live workspaces, install bases and the
    /// shared cache included — stopping any running server first.
    pub clear_all: bool,
    /// Worker threads for the fan-out over bases and cache files. `None` means one per logical
    /// core.
    pub jobs: Option<usize>,
}

impl BazelPruneOptions {
    /// Options for the conventional roots with the default age, the shape a sweep uses.
    #[must_use]
    pub fn conventional(dry_run: bool, one_file_system: bool) -> Self {
        Self {
            roots: conventional_roots(),
            dry_run,
            force: false,
            one_file_system,
            max_age: DEFAULT_MAX_AGE,
            clear_all: false,
            jobs: None,
        }
    }
}

/// What was proven about one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputBaseState {
    /// No marker was readable at either location. The same rule as everywhere else in the
    /// catalog: no marker, no claim, nothing removed.
    Unproven,
    /// The marker names a workspace that still exists and was built in recently. Left alone and
    /// reported by the path it named.
    Owned {
        /// The path the marker recorded.
        owner: PathBuf,
    },
    /// A Bazel server is still running against it. Never removed by age or orphaning.
    Running {
        /// The path the marker recorded, if any.
        owner: Option<PathBuf>,
    },
    /// The marker names a workspace that no longer exists anywhere on disk.
    Orphaned {
        /// The path the marker recorded.
        owner: PathBuf,
    },
    /// The marker names a directory that exists but is a git worktree whose administration is
    /// gone, so `git` no longer considers it a checkout of anything.
    Abandoned {
        /// The path the marker recorded.
        owner: PathBuf,
    },
    /// The workspace exists, but nothing has built there for longer than the age limit.
    Stale {
        /// The path the marker recorded, if the base has one. An unmarked child of a
        /// `_bazel_<user>` directory is Bazel's, and ages out the same way.
        owner: Option<PathBuf>,
        /// How long since the base was last built in.
        idle: Duration,
    },
    /// Removed because a full clear was asked for.
    Cleared {
        /// The path the marker recorded, if any.
        owner: Option<PathBuf>,
    },
    /// An extracted Bazel install (`<root>/install/<hash>`).
    InstallBase {
        /// Whether a surviving output base still points at it.
        in_use: bool,
    },
    /// The shared download cache (`<root>/cache`); entries older than the age limit go.
    Cache {
        /// How many files were old enough to remove.
        files: usize,
    },
}

impl OutputBaseState {
    /// Whether this state is one a removal was attempted against.
    #[must_use]
    pub fn is_removable(&self) -> bool {
        match self {
            Self::Orphaned { .. }
            | Self::Abandoned { .. }
            | Self::Stale { .. }
            | Self::Cleared { .. }
            | Self::InstallBase { in_use: false } => true,
            Self::Cache { files } => *files > 0,
            Self::Unproven | Self::Owned { .. } | Self::Running { .. } | Self::InstallBase { in_use: true } => false,
        }
    }

    /// The workspace the marker named, if it named one.
    #[must_use]
    pub fn owner(&self) -> Option<&Path> {
        match self {
            Self::Owned { owner } | Self::Orphaned { owner } | Self::Abandoned { owner } => Some(owner),
            Self::Running { owner } | Self::Cleared { owner } | Self::Stale { owner, .. } => owner.as_deref(),
            Self::Unproven | Self::InstallBase { .. } | Self::Cache { .. } => None,
        }
    }
}

/// One candidate output base and what became of it.
#[derive(Debug, Clone)]
pub struct OutputBase {
    /// The candidate's own directory — `<output_user_root>/_bazel_<user>/<hash>`.
    pub path: PathBuf,
    /// What was proven.
    pub state: OutputBaseState,
    /// Its size, measured only for a state removal was attempted against.
    pub bytes: Option<u64>,
    /// What happened, for a state removal was attempted against.
    pub outcome: Option<Outcome>,
}

impl OutputBase {
    /// Whether this output base was found to be removable, whatever became of the removal
    /// attempt.
    #[must_use]
    pub fn is_removable(&self) -> bool {
        self.state.is_removable()
    }
}

/// The footer's numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Candidates found across every root.
    pub found: usize,
    /// Removable, and actually removed.
    pub removed: usize,
    /// Removable, but a rail refused the removal.
    pub refused: usize,
    /// Not removable — owned, running, in use, or unproven.
    pub kept: usize,
    /// Bytes reclaimed, or that would be.
    pub bytes: u64,
}

/// Everything one round of housekeeping produced.
#[derive(Debug)]
pub struct BazelPruneResult {
    /// The roots that were searched, canonicalized. A root that did not exist on this machine —
    /// which is the ordinary case for at least some of the conventional defaults — is silently
    /// absent rather than listed, since absence there is not a usage error.
    pub roots: Vec<PathBuf>,
    /// Candidates, sorted by path.
    pub output_bases: Vec<OutputBase>,
    /// Whether removal was withheld.
    pub dry_run: bool,
    /// Wall-clock time.
    pub elapsed: Duration,
}

impl BazelPruneResult {
    /// The footer's numbers.
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals {
            found: self.output_bases.len(),
            ..Totals::default()
        };
        for base in &self.output_bases {
            match (base.is_removable(), &base.outcome) {
                (true, Some(Outcome::Removed | Outcome::WouldRemove)) => {
                    totals.removed += 1;
                    totals.bytes += base.bytes.unwrap_or(0);
                }
                (true, Some(Outcome::Refused(_))) => totals.refused += 1,
                _ => totals.kept += 1,
            }
        }
        totals
    }

    /// The process exit code: zero unless a removal was attempted and failed outright.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        let failed = self.output_bases.iter().any(|base| {
            matches!(
                base.outcome,
                Some(Outcome::Failed(_) | Outcome::PartiallyRemoved { .. })
            )
        });
        if failed {
            crate::report::exit::REMOVAL_FAILED
        } else {
            crate::report::exit::SUCCESS
        }
    }
}

/// The conventional output-user-roots to search when none were named.
///
/// A guess informed by one machine and Bazel's documented default locations, not a survey. A
/// root that does not exist here is the ordinary case, not a usage error — and [`prune`] treats
/// a root named explicitly the same way, since every one of these names a convention rather
/// than a tree the caller has confirmed exists.
///
/// `VOOM_BAZEL_ROOTS`, a platform path list (`:` on Unix), replaces the guess entirely; set
/// empty it names no roots at all. A sweep runs this stage unasked and `--clear-caches` makes it
/// destructive, so anything that runs the binary against a scratch tree — the test suite above
/// all — sets it rather than trusting the real machine's `/var/tmp` to be out of reach.
#[must_use]
pub fn conventional_roots() -> Vec<PathBuf> {
    if let Some(roots) = std::env::var_os(ROOTS_ENV) {
        return std::env::split_paths(&roots)
            .filter(|root| !root.as_os_str().is_empty())
            .collect();
    }
    let Some(user) = current_user() else {
        return Vec::new();
    };
    let name = format!("_bazel_{user}");
    let mut roots = vec![PathBuf::from("/tmp").join(&name), PathBuf::from("/var/tmp").join(&name)];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".cache/bazel").join(&name));
        roots.push(home.join("Library/Caches/bazel").join(&name));
    }
    roots
}

fn current_user() -> Option<String> {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).ok()
}

/// What a root's immediate child is.
enum Candidate {
    OutputBase(PathBuf),
    InstallBase(PathBuf),
    Cache(PathBuf),
}

/// Searches every root for output bases and removes the stale ones.
///
/// A root that does not exist or cannot be listed is silently absent from the result rather
/// than an error — the caller has already decided whether an empty root list is a problem
/// worth stopping for; here, the ordinary case is a machine that keeps its output bases
/// somewhere other conventional locations do not name.
///
/// # Errors
///
/// [`crate::error::Error::ReadDir`] if a root that does exist cannot be resolved to a canonical
/// path — a permission failure, not an absence.
pub fn prune(options: &BazelPruneOptions) -> Result<BazelPruneResult> {
    let started = Instant::now();
    let mut roots = Vec::new();
    let mut candidates = Vec::new();

    for root in &options.roots {
        let Ok(canonical) = root.canonicalize() else {
            continue;
        };
        candidates.extend(candidates_in(&canonical));
        roots.push(canonical);
    }

    let guard = Guard::new(&roots, options.one_file_system)?;
    let removal = Removal {
        dry_run: options.dry_run,
        force: options.force,
    };

    let mut output_bases = Vec::new();
    let mut install_bases = Vec::new();
    let mut caches = Vec::new();
    for candidate in candidates {
        match candidate {
            Candidate::OutputBase(path) => output_bases.push(path),
            Candidate::InstallBase(path) => install_bases.push(path),
            Candidate::Cache(path) => caches.push(path),
        }
    }

    // Every base is independent of the others, and each one is a handful of small reads followed
    // by a removal: latency-bound work that serial execution leaves the disk idle for. The
    // install bases wait for the output bases only because they ask which of them survived. The
    // shared cache is separate from both and runs beside them.
    let pool = build_pool(options.jobs)?;
    let (mut found, cache_found) = pool.install(|| {
        rayon::join(
            || {
                let mut found: Vec<OutputBase> = output_bases
                    .into_par_iter()
                    .map(|path| handle_output_base(path, options, &guard, removal))
                    .collect();

                // An install base is in use when a base that survived this run still points at
                // it. A dry run leaves everything on disk, so what *would* be removed is
                // excluded explicitly rather than by looking.
                let referenced: Vec<PathBuf> = found
                    .iter()
                    .filter(|base| {
                        !base.is_removable() || base.outcome.as_ref().is_some_and(|outcome| !outcome.is_reclaimed())
                    })
                    .filter_map(|base| std::fs::canonicalize(base.path.join("install")).ok())
                    .collect();
                found.par_extend(
                    install_bases
                        .into_par_iter()
                        .map(|path| handle_install_base(path, &referenced, options, &guard, removal)),
                );
                found
            },
            || {
                caches
                    .into_par_iter()
                    .map(|path| handle_cache(path, options, &guard, removal))
                    .collect::<Vec<_>>()
            },
        )
    });
    found.extend(cache_found);

    found.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(BazelPruneResult {
        roots,
        output_bases: found,
        dry_run: options.dry_run,
        elapsed: started.elapsed(),
    })
}

/// Builds the scoped worker pool for the fan-out, as `run.rs` does for the sweep.
fn build_pool(jobs: Option<usize>) -> Result<rayon::ThreadPool> {
    let jobs = jobs.map_or(0, |jobs| jobs.max(1));
    rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|source| crate::error::Error::ThreadPool { jobs, source })
}

/// One root's immediate subdirectories, with `install/` expanded one level: its children are
/// the install bases, and `cache/` is the shared download cache rather than an output base.
///
/// `install/` and `cache/` are Bazel's only under a Bazel output-user-root (`_bazel_<user>`);
/// under any other directory those names are ordinary directories, and a name alone never
/// proves an ecosystem. This is what lets `bazel-prune --all` be pointed at an unrelated
/// directory without clearing its `cache/` or `install/`.
fn candidates_in(root: &Path) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let bazel_root = is_bazel_user_root(root);
    for path in immediate_subdirectories(root) {
        match path.file_name().and_then(|name| name.to_str()) {
            Some("install") if bazel_root => {
                candidates.extend(immediate_subdirectories(&path).into_iter().map(Candidate::InstallBase));
            }
            Some("cache") if bazel_root => candidates.push(Candidate::Cache(path)),
            _ => candidates.push(Candidate::OutputBase(path)),
        }
    }
    candidates
}

/// Whether `root` is itself a Bazel output-user-root, named `_bazel_<user>` by Bazel.
fn is_bazel_user_root(root: &Path) -> bool {
    root.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("_bazel_"))
}

/// One root's immediate subdirectories — the only place an output base can be.
fn immediate_subdirectories(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

fn handle_output_base(path: PathBuf, options: &BazelPruneOptions, guard: &Guard, removal: Removal) -> OutputBase {
    let mut state = state_of(&path, options);

    // A full clear takes a live base too, but only after its server has exited: removing an
    // output base out from under a running server leaves it wedged on a tree that is gone. One
    // that will not exit is kept and reported as running.
    if let OutputBaseState::Cleared { owner } = &state
        && !options.dry_run
        && !stop_server(&path)
    {
        state = OutputBaseState::Running { owner: owner.clone() };
    }
    finish_removal(path, state, guard, removal)
}

fn finish_removal(path: PathBuf, state: OutputBaseState, guard: &Guard, removal: Removal) -> OutputBase {
    if !state.is_removable() {
        return OutputBase {
            path,
            state,
            bytes: None,
            outcome: None,
        };
    }
    let measured = measure_fully(&path);
    let outcome = guard.remove(&path, measured.bytes, removal);
    if matches!(outcome, Outcome::Removed)
        && let Some(owner) = state.owner()
    {
        unlink_convenience_symlinks(owner, &path);
    }
    OutputBase {
        path,
        state,
        bytes: Some(measured.bytes),
        outcome: Some(outcome),
    }
}

fn state_of(output_base: &Path, options: &BazelPruneOptions) -> OutputBaseState {
    let owner = read_owner(output_base);

    if !options.clear_all && running_server(output_base) {
        return OutputBaseState::Running { owner };
    }
    if options.clear_all && (owner.is_some() || is_a_bazel_base(output_base)) {
        return OutputBaseState::Cleared { owner };
    }

    let Some(owner) = owner else {
        return match idle_time(output_base) {
            Some(idle) if is_a_bazel_base(output_base) && idle > options.max_age => {
                OutputBaseState::Stale { owner: None, idle }
            }
            _ => OutputBaseState::Unproven,
        };
    };
    if !owner.exists() {
        return OutputBaseState::Orphaned { owner };
    }
    if is_dead_worktree(&owner) {
        return OutputBaseState::Abandoned { owner };
    }
    match idle_time(output_base) {
        Some(idle) if idle > options.max_age => OutputBaseState::Stale {
            owner: Some(owner),
            idle,
        },
        _ => OutputBaseState::Owned { owner },
    }
}

/// Whether the candidate lives directly under a `_bazel_<user>` directory — Bazel's own
/// naming, which is what makes an unmarked child of it Bazel's to clear.
fn is_bazel_root(output_base: &Path) -> bool {
    output_base
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("_bazel_"))
}

/// Whether a directory carries Bazel's own structure, not merely a `_bazel_`-named parent.
///
/// Bazel creates `execroot/` in every output base, so an unmarked directory without it is not
/// Bazel's and is left alone. This keeps the unmarked-child case from being a deletion decided
/// by a directory's name — the rule ADR 0002 forbids everywhere else.
fn is_a_bazel_base(output_base: &Path) -> bool {
    is_bazel_root(output_base) && output_base.join("execroot").is_dir()
}

/// Reads the owner marker, at its own root first and then nested under `execroot/` — see
/// [`OWNER_MARKER`] for why the second location is checked and not followed further.
fn read_owner(output_base: &Path) -> Option<PathBuf> {
    for candidate in [
        output_base.join(OWNER_MARKER),
        output_base.join("execroot").join(OWNER_MARKER),
    ] {
        if let Some(contents) = crate::io::read_capped_text(&candidate, CONTROL_FILE_MAX_BYTES) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    None
}

/// Whether the owner is a linked worktree whose git administration no longer exists.
///
/// A linked worktree's `.git` is a file reading `gitdir: <path>`; `git worktree remove` and
/// `git worktree prune` delete the target, and a directory that survives (untracked files,
/// a build symlink) is then a checkout of nothing. A plain repository has a `.git` directory
/// and a non-git workspace has neither, both of which are left alone.
fn is_dead_worktree(owner: &Path) -> bool {
    let Some(contents) = crate::io::read_capped_text(&owner.join(".git"), CONTROL_FILE_MAX_BYTES) else {
        return false;
    };
    let Some(target) = contents.trim().strip_prefix("gitdir:") else {
        return false;
    };
    let target = Path::new(target.trim());
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        owner.join(target)
    };
    !resolved.exists()
}

/// How long since Bazel last did anything in this output base.
fn idle_time(output_base: &Path) -> Option<Duration> {
    let newest = std::iter::once(output_base.to_path_buf())
        .chain(ACTIVITY_MARKERS.iter().map(|name| output_base.join(name)))
        .filter_map(|path| std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok())
        .max()?;
    std::time::SystemTime::now().duration_since(newest).ok()
}

/// The pid of a live Bazel server for this output base, if there is one.
///
/// A pid file outlives its server and the pid can be reused, so the file alone proves nothing:
/// only a process whose command line names this output base counts. A Bazel server's arguments
/// carry `--output_base=<path>`, so a match is the server and not an unrelated process that
/// merely mentions Bazel.
#[cfg(unix)]
fn server_pid(output_base: &Path) -> Option<u32> {
    let pid: u32 = crate::io::read_capped_text(&output_base.join(SERVER_PID_FILE), CONTROL_FILE_MAX_BYTES)?
        .trim()
        .parse()
        .ok()?;
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let command = String::from_utf8_lossy(&output.stdout);
    command.contains(&*output_base.to_string_lossy()).then_some(pid)
}

/// Without a portable process check, a recorded pid is treated as live: removing a base from
/// under a running server wedges it, so the conservative answer is "running". This makes a
/// sweep and `--clear-caches` more conservative on Windows, never less safe.
#[cfg(not(unix))]
fn server_pid(output_base: &Path) -> Option<u32> {
    crate::io::read_capped_text(&output_base.join(SERVER_PID_FILE), CONTROL_FILE_MAX_BYTES)?
        .trim()
        .parse()
        .ok()
}

fn running_server(output_base: &Path) -> bool {
    server_pid(output_base).is_some()
}

/// Asks the server to exit and waits for it. `true` once nothing is running against the base.
#[cfg(unix)]
fn stop_server(output_base: &Path) -> bool {
    let Some(pid) = server_pid(output_base) else {
        return true;
    };
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    let deadline = Instant::now() + SERVER_STOP_TIMEOUT;
    while Instant::now() < deadline {
        if server_pid(output_base).is_none() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Without a way to stop a server, never claim one stopped: `handle_output_base` keeps and
/// reports the base as running, which is the safe outcome.
#[cfg(not(unix))]
fn stop_server(_output_base: &Path) -> bool {
    false
}

/// Removes the `bazel-*` convenience symlinks in a workspace that point into a base that was
/// just removed, so the workspace is not left holding dangling links.
fn unlink_convenience_symlinks(owner: &Path, removed_base: &Path) {
    let Ok(entries) = std::fs::read_dir(owner) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("bazel-") {
            continue;
        }
        let path = entry.path();
        // Only an absolute target with no `..` can be trusted to sit inside the base by a
        // lexical prefix check; a relative or dot-dot target is left alone rather than followed.
        let points_into_base = std::fs::read_link(&path).is_ok_and(|target| {
            target.is_absolute()
                && !target
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
                && target.starts_with(removed_base)
        });
        if points_into_base {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn handle_install_base(
    path: PathBuf,
    referenced: &[PathBuf],
    options: &BazelPruneOptions,
    guard: &Guard,
    removal: Removal,
) -> OutputBase {
    let in_use = !options.clear_all
        && path
            .canonicalize()
            .is_ok_and(|canonical| referenced.contains(&canonical));
    finish_removal(path, OutputBaseState::InstallBase { in_use }, guard, removal)
}

/// The shared download cache: whole with `clear_all`, otherwise only files not modified within
/// the age limit (Bazel touches an entry each time it is served), with directories left empty
/// by that removed afterwards.
fn handle_cache(path: PathBuf, options: &BazelPruneOptions, guard: &Guard, removal: Removal) -> OutputBase {
    if options.clear_all {
        let files = count_files(&path);
        return finish_removal(path, OutputBaseState::Cache { files: files.max(1) }, guard, removal);
    }

    let now = std::time::SystemTime::now();
    let aged = collect_aged_files(&path, now, options.max_age);

    // One removal per file, the slowest part of the whole stage on a cache of tens of thousands
    // of entries, and independent per file.
    let results: Vec<(u64, Outcome)> = aged
        .par_iter()
        .map(|(file, size)| (*size, guard.remove(file, *size, removal)))
        .collect();
    let mut bytes = 0;
    let mut outcome = None;
    for (size, result) in results {
        if result.is_reclaimed() {
            bytes += size;
        }
        if outcome.is_none() || !result.is_reclaimed() {
            outcome = Some(result);
        }
    }
    if !removal.dry_run {
        remove_empty_directories(&path);
    }
    OutputBase {
        path,
        state: OutputBaseState::Cache { files: aged.len() },
        bytes: (!aged.is_empty()).then_some(bytes),
        outcome,
    }
}

/// The immediate children of `dir` with their file types, or nothing when it cannot be listed.
fn children(dir: &Path) -> Vec<(PathBuf, Option<std::fs::FileType>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| (entry.path(), entry.file_type().ok()))
        .collect()
}

fn count_files(root: &Path) -> usize {
    children(root)
        .into_par_iter()
        .map(|(path, file_type)| match file_type {
            Some(file_type) if file_type.is_dir() => count_files(&path),
            Some(file_type) if file_type.is_file() => 1,
            _ => 0,
        })
        .sum()
}

/// Every file under `dir` not modified within `max_age`, with its size. Subdirectories are
/// walked in parallel: a download cache is tens of thousands of entries, and each `metadata`
/// call is a round trip to the disk.
fn collect_aged_files(dir: &Path, now: std::time::SystemTime, max_age: Duration) -> Vec<(PathBuf, u64)> {
    children(dir)
        .into_par_iter()
        .flat_map_iter(|(path, file_type)| {
            let Some(file_type) = file_type else {
                return Vec::new();
            };
            if file_type.is_dir() {
                return collect_aged_files(&path, now, max_age);
            }
            if !file_type.is_file() {
                return Vec::new();
            }
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                return Vec::new();
            };
            let aged = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > max_age);
            if aged { vec![(path, metadata.len())] } else { Vec::new() }
        })
        .collect()
}

/// Removes directories a file removal left empty, deepest first. `remove_dir` refuses a
/// non-empty directory, which is the whole safety argument.
fn remove_empty_directories(root: &Path) {
    children(root).into_par_iter().for_each(|(path, file_type)| {
        if file_type.is_some_and(|file_type| file_type.is_dir()) {
            remove_empty_directories(&path);
            let _ = std::fs::remove_dir(&path);
        }
    });
}

#[cfg(test)]
mod tests;
