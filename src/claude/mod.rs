//! Claude Code job scratch — age-gated, and never trusted on `state.json`'s `state` alone.
//!
//! `~/.claude/jobs/<id>/tmp/` is free-form scratch a background job writes to, and nothing
//! reaps it once the job is done. `state.json`'s own `state` field lags reality: a job whose
//! daemon-managed worker was killed can be resumed later in an ordinary foreground process that
//! never touches the file again, and the measurement behind
//! `adrs/0015-claude-code-job-scratch.md` found exactly that — eight days of "stopped" while
//! the same session was, in fact, still running.
//!
//! So [`prune`] does not read `state`. It reads `lastTerminalAt` for age, checks the running
//! process list for the recorded session as a hard block that can only *prevent* a removal, and
//! removes nothing at all unless [`ClaudePruneOptions::remove`] was asked for on top of every
//! other condition holding — one flag more cautious than [`crate::bazel`], because a session's
//! liveness is a fact about intent that nothing on disk records reliably, where an output
//! base's owner is a filesystem fact a marker can just state.

mod report;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

pub use report::{document, render_human, render_json};
use serde::Deserialize;

use crate::delete::{Guard, Outcome, Removal};
use crate::error::Result;
use crate::size::measure_fully;

/// The JSON shape's version, versioned separately from the sweep's because it is a separate
/// document sharing only the envelope.
pub const SCHEMA_VERSION: u32 = 1;

/// The file a job directory carries at its root, read only for `sessionId` and
/// `lastTerminalAt` — `state` is deliberately never read; see the module doc.
const STATE_FILE_NAME: &str = "state.json";

/// The only thing removed from a job directory. `state.json` and the rest of a job's own
/// record are kilobytes against `tmp/`'s gigabytes, and are worth keeping regardless.
const SCRATCH_DIR_NAME: &str = "tmp";

/// The shortest quiet period this ADR found unsafe to go below, not a period confirmed long
/// enough — see `adrs/0015-claude-code-job-scratch.md`.
pub const DEFAULT_MIN_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

#[derive(Debug, Deserialize)]
struct State {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "lastTerminalAt")]
    last_terminal_at: Option<String>,
}

/// What to search, and under what conditions removal may proceed.
#[expect(
    clippy::struct_excessive_bools,
    reason = "these mirror command-line flags, which are bools by nature"
)]
#[derive(Debug, Clone)]
pub struct ClaudePruneOptions {
    /// `~/.claude/jobs`-shaped directories to search. Defaults to `~/.claude/jobs` when the
    /// caller passes none.
    pub roots: Vec<PathBuf>,
    /// How long `lastTerminalAt` must have passed before a job is even a candidate.
    pub min_age: Duration,
    /// The second, explicit opt-in removal requires on top of the age gate and the liveness
    /// check both holding. Without it, `voom claude-prune` only ever reports.
    pub remove: bool,
    /// Withhold the removal and report what would happen, once `remove` has already said to
    /// try.
    pub dry_run: bool,
    /// Retry a failed removal, repairing permissions inside the scratch directory first.
    pub force: bool,
    /// Whether removal stays on the root's filesystem.
    pub one_file_system: bool,
}

/// What was found about one job, in the order removal actually checks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    /// No `state.json`, or it named neither field this reads. The same rule as an unproven
    /// catalog artifact: no claim, nothing removed.
    Unproven,
    /// A process on this machine right now carries the recorded session on its command line.
    /// Hard-blocked regardless of age or flags — the one positive fact available.
    Live {
        /// The session the running process named.
        session_id: String,
    },
    /// No live process found, but `lastTerminalAt` is more recent than the minimum age.
    TooRecent {
        /// How long it has actually been quiet.
        age: Duration,
    },
    /// No live process, old enough, and eligible — subject to `remove` and `dry_run` deciding
    /// whether anything is actually attempted.
    Eligible {
        /// How long it has been quiet.
        age: Duration,
    },
}

/// One job directory and what became of its scratch.
#[derive(Debug, Clone)]
pub struct Job {
    /// The job directory itself — `state.json` lives here, alongside the scratch that may be
    /// removed.
    pub path: PathBuf,
    /// What was found.
    pub state: JobState,
    /// The scratch directory's size, measured only when removal was attempted.
    pub bytes: Option<u64>,
    /// What happened, only when removal was attempted.
    pub outcome: Option<Outcome>,
}

/// The footer's numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Job directories found across every root.
    pub found: usize,
    /// Eligible, and actually removed.
    pub removed: usize,
    /// Eligible, but a rail refused the removal.
    pub refused: usize,
    /// Everything else: unproven, live, too recent, or eligible with nothing attempted.
    pub kept: usize,
    /// Bytes reclaimed, or that would be.
    pub bytes: u64,
}

/// Everything one round produced.
#[derive(Debug)]
pub struct ClaudePruneResult {
    /// The roots searched, canonicalized.
    pub roots: Vec<PathBuf>,
    /// Jobs, sorted by path.
    pub jobs: Vec<Job>,
    /// Whether `--remove` was given at all.
    pub remove: bool,
    /// Whether removal was withheld.
    pub dry_run: bool,
    /// Wall-clock time.
    pub elapsed: Duration,
}

impl ClaudePruneResult {
    /// The footer's numbers.
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals {
            found: self.jobs.len(),
            ..Totals::default()
        };
        for job in &self.jobs {
            match (&job.state, &job.outcome) {
                (JobState::Eligible { .. }, Some(Outcome::Removed | Outcome::WouldRemove)) => {
                    totals.removed += 1;
                    totals.bytes += job.bytes.unwrap_or(0);
                }
                (JobState::Eligible { .. }, Some(Outcome::Refused(_))) => totals.refused += 1,
                _ => totals.kept += 1,
            }
        }
        totals
    }

    /// The process exit code: zero unless a removal was attempted and failed outright.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        let failed = self
            .jobs
            .iter()
            .any(|job| matches!(job.outcome, Some(Outcome::Failed(_) | Outcome::PartiallyRemoved { .. })));
        if failed {
            crate::report::exit::REMOVAL_FAILED
        } else {
            crate::report::exit::SUCCESS
        }
    }
}

/// `~/.claude/jobs`, if a home directory can be found.
#[must_use]
pub fn default_root() -> Vec<PathBuf> {
    dirs::home_dir().map_or_else(Vec::new, |home| vec![home.join(".claude/jobs")])
}

/// Searches every root for job directories and removes the eligible ones' scratch, if asked to.
///
/// A root that does not exist is silently absent from the result rather than an error — the
/// same reasoning as [`crate::bazel::prune`]: this names a convention, not a tree the caller
/// has necessarily confirmed exists.
///
/// # Errors
///
/// [`crate::error::Error::ReadDir`] if a root that does exist cannot be resolved to a canonical
/// path.
pub fn prune(options: &ClaudePruneOptions) -> Result<ClaudePruneResult> {
    let started = Instant::now();
    let mut roots = Vec::new();
    let mut candidates = Vec::new();

    for root in &options.roots {
        let Ok(canonical) = root.canonicalize() else {
            continue;
        };
        candidates.extend(immediate_subdirectories(&canonical));
        roots.push(canonical);
    }

    let guard = Guard::new(&roots, options.one_file_system)?;
    let removal = Removal {
        dry_run: options.dry_run,
        force: options.force,
    };
    let now = SystemTime::now();

    let mut jobs: Vec<Job> = candidates
        .into_iter()
        .map(|path| {
            let state = state_of(&path, options.min_age, now);
            let scratch = path.join(SCRATCH_DIR_NAME);
            let (bytes, outcome) = if matches!(state, JobState::Eligible { .. }) && options.remove && scratch.is_dir() {
                let measured = measure_fully(&scratch);
                (
                    Some(measured.bytes),
                    Some(guard.remove(&scratch, measured.bytes, removal)),
                )
            } else {
                (None, None)
            };
            Job {
                path,
                state,
                bytes,
                outcome,
            }
        })
        .collect();

    jobs.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(ClaudePruneResult {
        roots,
        jobs,
        remove: options.remove,
        dry_run: options.dry_run,
        elapsed: started.elapsed(),
    })
}

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

fn state_of(job_dir: &Path, min_age: Duration, now: SystemTime) -> JobState {
    let Some(state) = read_state(job_dir) else {
        return JobState::Unproven;
    };
    let Some(last_terminal_at) = state.last_terminal_at.as_deref().and_then(parse_utc_timestamp) else {
        return JobState::Unproven;
    };
    if let Some(session_id) = state.session_id.as_deref()
        && is_resumed(session_id)
    {
        return JobState::Live {
            session_id: session_id.to_owned(),
        };
    }
    let age = now.duration_since(last_terminal_at).unwrap_or_default();
    if age < min_age {
        JobState::TooRecent { age }
    } else {
        JobState::Eligible { age }
    }
}

fn read_state(job_dir: &Path) -> Option<State> {
    let contents = std::fs::read_to_string(job_dir.join(STATE_FILE_NAME)).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Whether any process on this machine has the session on its command line right now.
///
/// This can only prove liveness, never the absence of it: a plain `claude` session with no
/// explicit `--resume` leaves no session id on its command line at all, and a miss here means
/// only that this particular check found nothing — not that the job is safe to remove. See
/// `adrs/0015-claude-code-job-scratch.md`.
fn is_resumed(session_id: &str) -> bool {
    let Ok(output) = Command::new("ps").args(["-A", "-o", "command="]).output() else {
        // `ps` is not available, or would not run — there is no platform-independent way to
        // ask. This is not "confirmed dead": it is the same "nothing found" this function
        // returns when `ps` runs and simply has no match, which leaves the age gate and
        // `--remove` as the only rails standing between a job and removal, exactly as the
        // module doc says a miss always does.
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.contains(session_id))
}

/// Parses the fixed shape Claude Code writes: `YYYY-MM-DDTHH:MM:SS(.fff)?Z`, UTC only.
///
/// Not a general RFC 3339 parser — there is exactly one format to read here, and rejecting
/// anything else is the same "no marker, no claim" rule the rest of this tool applies to a
/// file it cannot make sense of.
fn parse_utc_timestamp(text: &str) -> Option<SystemTime> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }

    let (hms, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let mut time_parts = hms.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let minute: u64 = time_parts.next()?.parse().ok()?;
    let second: u64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() {
        return None;
    }
    let millis: u64 = format!("{fraction:0<3}").get(..3)?.parse().ok()?;

    let days = days_from_civil(year, month, day)?;
    let seconds = days
        .checked_mul(86400)?
        .checked_add((hour * 3600 + minute * 60 + second).cast_signed())?;
    if seconds < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds.cast_unsigned()) + Duration::from_millis(millis))
}

/// Days since the Unix epoch for a UTC calendar date, Howard Hinnant's `days_from_civil`.
///
/// Closed-form and exact over the whole proleptic Gregorian calendar, which is more range than
/// this ever needs — every timestamp read here is a Claude Code job's own recent activity — but
/// it is the well-known formula rather than a hand-rolled leap-year special case, and is
/// verified against known dates below rather than trusted on inspection alone.
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tree;

    fn options(root: &Path) -> ClaudePruneOptions {
        ClaudePruneOptions {
            roots: vec![root.to_path_buf()],
            min_age: DEFAULT_MIN_AGE,
            remove: true,
            dry_run: false,
            force: false,
            one_file_system: true,
        }
    }

    fn write_state(job_dir: &Path, session_id: &str, last_terminal_at: &str) {
        std::fs::write(
            job_dir.join("state.json"),
            format!(r#"{{"sessionId": "{session_id}", "lastTerminalAt": "{last_terminal_at}"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn should_convert_a_known_calendar_date() {
        assert_eq!(days_from_civil(1970, 1, 1), Some(0));
        assert_eq!(days_from_civil(2026, 9, 10), Some(20706));
    }

    #[test]
    fn should_parse_the_exact_shape_claude_code_writes() {
        let parsed = parse_utc_timestamp("2026-09-10T05:13:43.668Z").expect("a valid timestamp");
        let expected = SystemTime::UNIX_EPOCH
            + Duration::from_secs(20706 * 86400 + 5 * 3600 + 13 * 60 + 43)
            + Duration::from_millis(668);
        assert_eq!(parsed, expected);
    }

    #[test]
    fn should_reject_a_shape_it_does_not_recognise() {
        assert_eq!(parse_utc_timestamp("2026-09-10 05:13:43"), None, "no `T`, no `Z`");
        assert_eq!(parse_utc_timestamp("not a date"), None);
    }

    /// The regression this ADR exists to prevent: a job whose `state.json` says "stopped" but
    /// whose session a running process still names must never be removed, however old the
    /// timestamp, however explicitly `--remove` was given.
    #[test]
    fn should_never_remove_a_job_a_running_process_still_names() {
        let fixture = tree(&["jobs/old-but-live/tmp/scratch.bin"]);
        let root = fixture.path().join("jobs");
        // This process's own command line is a session id `ps` will really find, which is the
        // point: the check is a real `ps` invocation, not a stub, and this proves it holds even
        // for a session with no age gate protecting it at all.
        let real_session = std::env::current_exe().unwrap().display().to_string();
        write_state(&root.join("old-but-live"), &real_session, "2000-01-01T00:00:00.000Z");

        let result = prune(&options(&root)).expect("the root resolves");

        let job = &result.jobs[0];
        assert!(matches!(&job.state, JobState::Live { .. }), "{:?}", job.state);
        assert!(job.outcome.is_none(), "nothing is even attempted against a live job");
        assert!(root.join("old-but-live/tmp").exists());
    }

    #[test]
    fn should_keep_a_job_more_recent_than_the_minimum_age() {
        let fixture = tree(&["jobs/fresh/tmp/scratch.bin"]);
        let root = fixture.path().join("jobs");
        let now = humantime_like_now();
        write_state(&root.join("fresh"), "session-nobody-is-running", &now);

        let result = prune(&options(&root)).expect("the root resolves");

        assert!(matches!(&result.jobs[0].state, JobState::TooRecent { .. }));
        assert!(root.join("fresh/tmp").exists());
    }

    #[test]
    fn should_remove_an_old_unresumed_jobs_scratch_when_remove_is_given() {
        let fixture = tree(&["jobs/ancient/tmp/scratch.bin", "jobs/ancient/timeline.jsonl"]);
        let root = fixture.path().join("jobs");
        write_state(
            &root.join("ancient"),
            "session-nobody-is-running",
            "2000-01-01T00:00:00.000Z",
        );

        let result = prune(&options(&root)).expect("the root resolves");

        let job = &result.jobs[0];
        assert!(matches!(&job.state, JobState::Eligible { .. }));
        assert_eq!(job.outcome, Some(Outcome::Removed));
        assert!(!root.join("ancient/tmp").exists(), "the scratch is gone");
        assert!(
            root.join("ancient/timeline.jsonl").exists(),
            "the job's own record survives"
        );
    }

    #[test]
    fn should_report_only_without_the_remove_flag() {
        let fixture = tree(&["jobs/ancient/tmp/scratch.bin"]);
        let root = fixture.path().join("jobs");
        write_state(
            &root.join("ancient"),
            "session-nobody-is-running",
            "2000-01-01T00:00:00.000Z",
        );

        let mut report_only = options(&root);
        report_only.remove = false;
        let result = prune(&report_only).expect("the root resolves");

        let job = &result.jobs[0];
        assert!(matches!(&job.state, JobState::Eligible { .. }));
        assert!(job.outcome.is_none(), "nothing is attempted without --remove");
        assert!(root.join("ancient/tmp").exists());
    }

    #[test]
    fn should_leave_an_unparsable_job_unproven() {
        let fixture = tree(&["jobs/broken/state.json"]);
        let root = fixture.path().join("jobs");
        std::fs::write(root.join("broken/state.json"), "not json").unwrap();

        let result = prune(&options(&root)).expect("the root resolves");

        assert_eq!(result.jobs[0].state, JobState::Unproven);
    }

    /// A timestamp near "now" for a fixture, without adding a dependency merely to format one.
    fn humantime_like_now() -> String {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        // `parse_utc_timestamp`'s own inverse would be a second implementation to keep in sync;
        // this instead just asks for a date far enough in the future that no `--min-age` this
        // test uses could ever call it old, which is all the fixture needs to be true.
        let days = now.as_secs() / 86400 + 3650;
        let year = 1970 + days / 365;
        format!("{year}-06-15T00:00:00.000Z")
    }
}
