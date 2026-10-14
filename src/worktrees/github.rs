//! Optional hosted merge proof: an exact HEAD on a merged pull request, never a branch name.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

pub(super) fn repository(main: &Path) -> Result<String, String> {
    let origin = super::git(main, &["remote", "get-url", "origin"])
        .ok_or_else(|| "GitHub merge proof requires an origin remote".to_owned())?;
    parse_repository(origin.trim()).ok_or_else(|| "origin is not a supported github.com repository URL".to_owned())
}

pub(super) fn parse_repository(origin: &str) -> Option<String> {
    let path = origin
        .strip_prefix("https://github.com/")
        .or_else(|| origin.strip_prefix("git@github.com:"))
        .or_else(|| origin.strip_prefix("ssh://git@github.com/"))?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let components: Vec<_> = path.split('/').collect();
    if components.len() != 2
        || components.iter().any(|part| {
            part.is_empty()
                || matches!(*part, "." | "..")
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
    {
        return None;
    }
    Some(path.to_owned())
}

pub(super) fn merged(main: &Path, repository: &str, head: &str) -> Result<bool, String> {
    if !matches!(head.len(), 40 | 64) || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("worktree HEAD is not a commit SHA".to_owned());
    }
    let mut command = Command::new("gh");
    command
        .current_dir(main)
        .args([
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            &format!("repos/{repository}/commits/{head}/pulls?per_page=100"),
        ])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let response = response(&mut command, TIMEOUT)?;
    merged_response(&response, repository, head)
}

fn response(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("GitHub merge proof: {error}"))?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("GitHub response pipe is unavailable".to_owned());
    };
    // Drain concurrently: waiting before reading deadlocks when gh fills the pipe.
    let deadline = Instant::now() + timeout;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let response = stdout.take(MAX_RESPONSE + 1).read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(response);
    });
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let bytes = receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|_| "GitHub response reader timed out or failed".to_owned())?
                    .map_err(|error| format!("GitHub response: {error}"))?;
                if !status.success() {
                    return Err("GitHub merge proof failed (gh authentication, API, or network error)".to_owned());
                }
                if bytes.len() as u64 > MAX_RESPONSE {
                    return Err("GitHub response exceeds the size limit".to_owned());
                }
                return Ok(bytes);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("GitHub merge proof timed out".to_owned());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("GitHub merge proof: {error}"));
            }
        }
    }
}

pub(super) fn merged_response(response: &[u8], repository: &str, head: &str) -> Result<bool, String> {
    let pulls: Vec<serde_json::Value> =
        serde_json::from_slice(response).map_err(|_| "invalid GitHub pull request response".to_owned())?;
    Ok(pulls.iter().any(|pull| {
        pull["head"]["sha"].as_str() == Some(head)
            && pull["merged_at"].as_str().is_some_and(|date| !date.is_empty())
            && pull["base"]["repo"]["full_name"]
                .as_str()
                .is_some_and(|base| base.eq_ignore_ascii_case(repository))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_validate_the_origin_repository_without_accepting_other_hosts_or_paths() {
        for origin in [
            "https://github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo",
        ] {
            assert_eq!(parse_repository(origin).as_deref(), Some("owner/repo"));
        }
        for origin in [
            "https://github.com.evil/owner/repo",
            "https://evil/github.com/owner/repo",
            "https://github.com/owner/../repo",
            "https://github.com/owner/repo?token=x",
            "https://github.com/owner/repo/extra",
        ] {
            assert!(parse_repository(origin).is_none(), "{origin}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn should_bound_response_reading_when_a_descendant_keeps_the_pipe_open() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 1 &"]).stdout(Stdio::piped());
        let started = Instant::now();
        assert!(
            response(&mut command, Duration::from_millis(30))
                .unwrap_err()
                .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn should_bound_a_hung_github_lookup() {
        let mut command = Command::new("sleep");
        command.arg("10").stdout(Stdio::piped());
        let started = Instant::now();
        assert!(
            response(&mut command, Duration::from_millis(30))
                .unwrap_err()
                .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
