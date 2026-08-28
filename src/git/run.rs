//! Invoking `git`, and reading what it said.
//!
//! Split from the step sequence because the two answer different questions: this file knows how
//! to start a subprocess, bound it in time, and turn its output into one line of English, and
//! knows nothing about worktrees or repacking.
//!
//! The timeout is the reason this is not four lines of [`Command::output`]. `Command` has none,
//! and a `git` against a repository on a stalled network mount does not return — one such
//! repository would wedge a sweep of a home directory indefinitely.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use humansize::{DECIMAL, format_size};

use super::{GitError, Step, StepKind, StepOutcome};

/// The program voom delegates to, resolved through `PATH` so a user's own git wins.
const GIT: &str = "git";

/// How often the wait loop asks whether the child has finished.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A finished child process.
#[derive(Debug)]
struct Completed {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

pub(super) fn ensure_git_available(timeout: Duration) -> Result<(), GitError> {
    let mut command = Command::new(GIT);
    command.arg("--version");
    match run_command(&mut command, Instant::now() + timeout) {
        Ok(_) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Err(GitError::NotInstalled),
        Err(source) => Err(GitError::Spawn { source }),
    }
}

pub(super) fn run_step(path: &Path, kind: StepKind, remote: Option<String>, args: &[&str], deadline: Instant) -> Step {
    let mut command = Command::new(GIT);
    command.current_dir(path).args(args);

    let mut recorded = vec![GIT.to_owned()];
    recorded.extend(args.iter().map(|arg| (*arg).to_owned()));

    let outcome = match run_command(&mut command, deadline) {
        Ok(completed) if completed.timed_out => StepOutcome::TimedOut,
        Ok(completed) if completed.code == Some(0) => StepOutcome::Succeeded {
            summary: summarize(kind, &completed),
        },
        Ok(completed) => StepOutcome::Failed {
            code: completed.code,
            message: first_lines(&completed.stderr, &completed.stdout),
        },
        // A spawn failure that is not "git is missing" — a fork limit, a working directory that
        // went away — is this repository's problem and not the run's.
        Err(source) => StepOutcome::Failed {
            code: None,
            message: source.to_string(),
        },
    };

    Step {
        kind,
        remote,
        command: recorded,
        outcome,
    }
}

/// Runs a command to completion, killing it if the deadline passes first.
///
/// [`Command`] has no timeout, so this is `spawn` plus a bounded `try_wait` poll. Both output
/// streams are drained by their own threads rather than after the wait: a child that fills a pipe
/// buffer blocks on the write, and a parent that is not reading would then poll to the deadline
/// and kill a process that had already done its work.
fn run_command(command: &mut Command, deadline: Instant) -> io::Result<Completed> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A credential or passphrase prompt would sit there until the deadline killed it.
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()?;

    let stdout = reader(child.stdout.take());
    let stderr = reader(child.stderr.take());
    let (code, timed_out) = wait_with_deadline(&mut child, deadline)?;

    Ok(Completed {
        code,
        stdout: stdout.map(join_reader).unwrap_or_default(),
        stderr: stderr.map(join_reader).unwrap_or_default(),
        timed_out,
    })
}

fn reader<R: Read + Send + 'static>(stream: Option<R>) -> Option<std::thread::JoinHandle<String>> {
    stream.map(|mut stream| {
        std::thread::spawn(move || {
            let mut text = String::new();
            // Output voom cannot read is not a reason to fail the step; git's exit status is.
            let _ = stream.read_to_string(&mut text);
            text
        })
    })
}

fn join_reader(handle: std::thread::JoinHandle<String>) -> String {
    handle.join().unwrap_or_default()
}

/// Waits for the child, killing it once the deadline has passed. Returns its code, and whether it
/// was killed.
fn wait_with_deadline(child: &mut Child, deadline: Instant) -> io::Result<(Option<i32>, bool)> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status.code(), false));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let status = child.wait()?;
            return Ok((status.code(), true));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

// ---------------------------------------------------------------------------------------------
// Reading git's output
// ---------------------------------------------------------------------------------------------

fn summarize(kind: StepKind, completed: &Completed) -> String {
    let output = format!("{}{}", completed.stdout, completed.stderr);
    match kind {
        StepKind::WorktreePrune => count_summary(&output, "Removing", "stale worktree"),
        StepKind::RemotePrune => count_summary(&output, "prune", "gone branch"),
        // The listing is not housekeeping: its "summary" is the raw output, which `prune_remotes`
        // parses and then discards along with the step.
        StepKind::RemoteList => completed.stdout.clone(),
        // `gc --auto` below its thresholds says nothing, which is the normal case and the reason
        // a sweep over a home directory prints nothing about git.
        StepKind::Gc => String::new(),
        StepKind::CountObjects => count_objects_summary(&completed.stdout),
    }
}

/// `N <noun>s pruned`, or nothing when git reported none.
///
/// Both `worktree prune -v` and `remote prune` name each thing they remove on its own line, and
/// `-n` names each thing they would have removed in the same shape. Counting the lines is what
/// lets one summary serve a real run and a dry one.
fn count_summary(output: &str, needle: &str, noun: &str) -> String {
    let count = output.lines().filter(|line| line.contains(needle)).count();
    if count == 0 {
        return String::new();
    }
    format!("{count} {} pruned", plural(count, noun))
}

/// What a repack would have had to work with, from `count-objects -v`.
///
/// Printed instead of running one, so a dry run still tells the reader whether there is anything
/// here worth a real one. Sizes are reported by git in KiB.
fn count_objects_summary(stdout: &str) -> String {
    let fields: BTreeMap<&str, u64> = stdout
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter_map(|(key, value)| value.trim().parse().ok().map(|value| (key.trim(), value)))
        .collect();
    let field = |name: &str| fields.get(name).copied().unwrap_or(0);
    let (loose, packed, packs) = (field("count"), field("in-pack"), field("packs"));
    format!(
        "gc withheld; {loose} loose {} ({}), {packed} packed in {packs} {} ({})",
        plural_u64(loose, "object"),
        format_size(field("size") * 1024, DECIMAL),
        plural_u64(packs, "pack"),
        format_size(field("size-pack") * 1024, DECIMAL),
    )
}

/// git's own complaint, trimmed to something that fits a report line.
fn first_lines(stderr: &str, stdout: &str) -> String {
    let source = if stderr.trim().is_empty() { stdout } else { stderr };
    let message: Vec<&str> = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(2)
        .collect();
    if message.is_empty() {
        "git failed without saying why".to_owned()
    } else {
        message.join("; ")
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_owned()
    } else {
        format!("{noun}s")
    }
}

fn plural_u64(count: u64, noun: &str) -> String {
    plural(usize::try_from(count).unwrap_or(usize::MAX), noun)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_count_what_a_worktree_prune_reported() {
        let output = "Removing worktrees/one: gitdir file points to non-existent location\n\
                      Removing worktrees/two: gitdir file points to non-existent location\n";
        assert_eq!(
            count_summary(output, "Removing", "stale worktree"),
            "2 stale worktrees pruned"
        );
    }

    #[test]
    fn should_say_nothing_when_git_pruned_nothing() {
        assert_eq!(count_summary("", "Removing", "stale worktree"), "");
    }

    #[test]
    fn should_read_the_object_counts_a_dry_run_reports_instead_of_repacking() {
        let stdout = "count: 12\nsize: 48\nin-pack: 340\npacks: 2\nsize-pack: 1234\ngarbage: 0\n";
        let summary = count_objects_summary(stdout);
        assert!(summary.contains("12 loose objects"), "{summary}");
        assert!(summary.contains("340 packed in 2 packs"), "{summary}");
    }

    /// `Command` has no timeout of its own, so this is the rail that keeps a hung git on a
    /// network mount from wedging a run. Exercised against `sleep` rather than git, because what
    /// is under test is the wait loop and not git.
    #[test]
    #[cfg(unix)]
    fn should_kill_a_command_that_outlives_its_deadline() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let started = Instant::now();

        let completed =
            run_command(&mut command, Instant::now() + Duration::from_millis(200)).expect("sleep exists on unix");

        assert!(completed.timed_out, "the deadline must be enforced, not merely noticed");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the child was killed rather than waited out: {:?}",
            started.elapsed()
        );
    }

    #[test]
    #[cfg(unix)]
    fn should_not_report_a_timeout_for_a_command_that_finished() {
        let mut command = Command::new("true");
        let completed = run_command(&mut command, Instant::now() + Duration::from_secs(30)).expect("true exists");
        assert!(!completed.timed_out);
        assert_eq!(completed.code, Some(0));
    }
}
