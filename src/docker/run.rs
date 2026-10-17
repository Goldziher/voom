//! Invoking `docker` and the VM tools beside it, and reading what they said.
//!
//! Split from the planning for the reason `git/run.rs` is: this file knows how to start a
//! subprocess, bound it in time and sort a failure into the few kinds that matter, and knows
//! nothing about images or volumes.
//!
//! The timeout is not decoration. A `docker` whose daemon is wedged — a Colima VM mid-boot, a
//! remote context behind a dead VPN — does not return, and a sweep that waited on it would never
//! finish. [`Command`] has no timeout, so this is `spawn` plus a bounded `try_wait` poll.

use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long the wait loop sleeps before asking again the first time.
const POLL_START: Duration = Duration::from_micros(200);

/// The ceiling the backoff doubles up to. A prune runs for seconds; there is nothing to gain from
/// asking it four hundred times a second.
const POLL_CEILING: Duration = Duration::from_millis(25);

/// The longest a one-line diagnostic is allowed to be before it is cut.
const MESSAGE_MAX_CHARS: usize = 200;

/// Why a command did not produce output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    /// The program is not on `PATH`.
    #[error("`{program}` is not installed")]
    NotInstalled {
        /// The program that was looked for.
        program: String,
    },
    /// The daemon's socket refused the connection or does not exist.
    #[error("the docker daemon is not running")]
    DaemonDown,
    /// The socket exists, and this user may not use it.
    #[error("permission denied talking to the docker daemon")]
    PermissionDenied,
    /// The command was killed at its deadline.
    #[error("`{program}` did not finish within {seconds}s")]
    TimedOut {
        /// The program that was killed.
        program: String,
        /// The deadline it missed, in whole seconds.
        seconds: u64,
    },
    /// It ran and said no, or could not be started for another reason.
    #[error("{message}")]
    Failed {
        /// The first line it wrote to stderr, or the spawn error.
        message: String,
    },
}

/// Runs `program` to completion, returning its stdout.
///
/// # Errors
///
/// A [`Failure`] naming the kind of problem: the program missing, the daemon unreachable, a
/// permission refusal, a missed deadline, or any other non-zero exit with its first stderr line.
pub fn run(program: &str, args: &[String], timeout: Duration) -> Result<String, Failure> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A hint is advice for a human at a terminal, and lands in the output voom parses.
        .env("DOCKER_CLI_HINTS", "false");

    let mut child = command.spawn().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => Failure::NotInstalled {
            program: program.to_owned(),
        },
        _ => Failure::Failed {
            message: error.to_string(),
        },
    })?;

    let stdout = reader(child.stdout.take());
    let stderr = reader(child.stderr.take());
    let outcome = wait_with_deadline(&mut child, Instant::now() + timeout);
    let stdout = stdout.map(join_reader).unwrap_or_default();
    let stderr = stderr.map(join_reader).unwrap_or_default();

    match outcome {
        Err(error) => Err(Failure::Failed {
            message: error.to_string(),
        }),
        Ok(None) => Err(Failure::TimedOut {
            program: program.to_owned(),
            seconds: timeout.as_secs(),
        }),
        Ok(Some(true)) => Ok(stdout),
        Ok(Some(false)) => Err(classify(&stderr)),
    }
}

/// Sorts a daemon's complaint into a kind, by the wording every client version has used for it.
#[must_use]
pub fn classify(stderr: &str) -> Failure {
    let lowered = stderr.to_ascii_lowercase();
    if lowered.contains("permission denied") {
        Failure::PermissionDenied
    } else if lowered.contains("cannot connect to the docker daemon")
        || lowered.contains("is the docker daemon running")
        || lowered.contains("connection refused")
    {
        Failure::DaemonDown
    } else {
        Failure::Failed {
            message: first_line(stderr),
        }
    }
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("failed");
    line.chars().take(MESSAGE_MAX_CHARS).collect()
}

fn reader<R: Read + Send + 'static>(stream: Option<R>) -> Option<std::thread::JoinHandle<String>> {
    stream.map(|mut stream| {
        std::thread::spawn(move || {
            let mut text = String::new();
            // Output voom cannot read is not a reason to fail the command; its exit status is.
            let _ = stream.read_to_string(&mut text);
            text
        })
    })
}

fn join_reader(handle: std::thread::JoinHandle<String>) -> String {
    handle.join().unwrap_or_default()
}

/// Waits for the child. `Some(success)` when it exited, `None` when it was killed at the deadline.
fn wait_with_deadline(child: &mut Child, deadline: Instant) -> io::Result<Option<bool>> {
    let mut wait = POLL_START;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status.success()));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        std::thread::sleep(wait);
        wait = (wait * 2).min(POLL_CEILING);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_classify_a_stopped_daemon() {
        let stderr = "Cannot connect to the Docker daemon at unix:///x.sock. Is the docker daemon running?";
        assert_eq!(classify(stderr), Failure::DaemonDown);
    }

    #[test]
    fn should_classify_a_socket_permission_refusal() {
        let stderr =
            "permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock";
        assert_eq!(classify(stderr), Failure::PermissionDenied);
    }

    #[test]
    fn should_keep_only_the_first_line_of_any_other_failure() {
        let failure = classify("\n  Error response from daemon: boom\nsecond line\n");
        assert_eq!(
            failure,
            Failure::Failed {
                message: "Error response from daemon: boom".to_owned()
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn should_report_a_missing_program_as_not_installed() {
        let failure = run("voom-no-such-program", &[], Duration::from_secs(5)).unwrap_err();
        assert!(matches!(failure, Failure::NotInstalled { .. }), "{failure:?}");
    }

    #[cfg(unix)]
    #[test]
    fn should_kill_a_command_that_outlives_its_deadline() {
        let args = vec!["5".to_owned()];
        let started = Instant::now();
        let failure = run("sleep", &args, Duration::from_millis(100)).unwrap_err();
        assert!(matches!(failure, Failure::TimedOut { .. }), "{failure:?}");
        assert!(started.elapsed() < Duration::from_secs(4), "it was killed, not awaited");
    }
}
