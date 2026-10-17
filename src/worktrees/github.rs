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

/// What GitHub says about a worktree's exact `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hosted {
    /// A merged pull request in the origin repository has exactly this head.
    Merged,
    /// No merged pull request is associated with this `HEAD`.
    NotMerged,
    /// A pull request merged, but its head is not this commit: the branch was amended, rebased or
    /// extended after the pull request, or this checkout holds commits GitHub never saw.
    HeadDiffers {
        /// The pull request's number.
        pull: u64,
    },
}

/// What a `gh api` call came back with.
#[derive(Debug, PartialEq, Eq)]
enum Reply {
    Body(Vec<u8>),
    /// HTTP 422 "No commit found for SHA": the commit exists only locally, so no pull request
    /// can contain it. An answer, not a failure.
    UnknownCommit,
}

pub(super) fn merged(main: &Path, repository: &str, head: &str, branch: Option<&str>) -> Result<Hosted, String> {
    if !matches!(head.len(), 40 | 64) || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("worktree HEAD is not a commit SHA".to_owned());
    }
    let reply = response(
        &mut api(
            main,
            &format!("repos/{repository}/commits/{head}/pulls?per_page=100"),
            &[],
        ),
        TIMEOUT,
    )?;
    let Reply::Body(body) = reply else {
        // A commit GitHub has never seen cannot be the head of a merged pull request, but the
        // branch it sits on may have been: squash-merged, then amended locally. One more lookup,
        // by branch, only so the report can say so instead of a bare "not merged".
        let Some(branch) = branch else {
            return Ok(Hosted::NotMerged);
        };
        let owner = repository.split('/').next().unwrap_or_default();
        let by_branch = response(
            &mut api(
                main,
                &format!("repos/{repository}/pulls"),
                &[
                    "-f",
                    "state=closed",
                    "-f",
                    &format!("head={owner}:{branch}"),
                    "-f",
                    "per_page=100",
                ],
            ),
            TIMEOUT,
        )?;
        return match by_branch {
            Reply::Body(body) => classify(&body, repository, head),
            Reply::UnknownCommit => Ok(Hosted::NotMerged),
        };
    };
    classify(&body, repository, head)
}

fn api(main: &Path, endpoint: &str, fields: &[&str]) -> Command {
    let mut command = Command::new("gh");
    command
        .current_dir(main)
        .args(["api", "--hostname", "github.com", "--method", "GET", endpoint])
        .args(fields)
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Whether a failed call's body is GitHub's 422 for a commit it does not have. `gh` prints the
/// error document on stdout, so the body is the only signal that survives with stderr discarded.
fn is_unknown_commit(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body).is_ok_and(|error| {
        error["status"].as_str() == Some("422")
            && error["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("No commit found for SHA"))
    })
}

fn response(command: &mut Command, timeout: Duration) -> Result<Reply, String> {
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
                    if is_unknown_commit(&bytes) {
                        return Ok(Reply::UnknownCommit);
                    }
                    return Err("GitHub merge proof failed (gh authentication, API, or network error)".to_owned());
                }
                if bytes.len() as u64 > MAX_RESPONSE {
                    return Err("GitHub response exceeds the size limit".to_owned());
                }
                return Ok(Reply::Body(bytes));
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

/// Reads a pull request list: exact merged head wins, a merged pull request with another head is
/// reported as such, anything else is not merged. Only pull requests whose base is `repository`
/// count, so a fork's pull request proves nothing here.
pub(super) fn classify(response: &[u8], repository: &str, head: &str) -> Result<Hosted, String> {
    let pulls: Vec<serde_json::Value> =
        serde_json::from_slice(response).map_err(|_| "invalid GitHub pull request response".to_owned())?;
    let mut differing = None;
    for pull in pulls.iter().filter(|pull| {
        pull["merged_at"].as_str().is_some_and(|date| !date.is_empty())
            && pull["base"]["repo"]["full_name"]
                .as_str()
                .is_some_and(|base| base.eq_ignore_ascii_case(repository))
    }) {
        if pull["head"]["sha"].as_str() == Some(head) {
            return Ok(Hosted::Merged);
        }
        differing = differing.or(pull["number"].as_u64());
    }
    Ok(differing.map_or(Hosted::NotMerged, |pull| Hosted::HeadDiffers { pull }))
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
    fn replying(script: &str) -> Result<Reply, String> {
        let mut command = Command::new("sh");
        command.args(["-c", script]).stdout(Stdio::piped());
        response(&mut command, Duration::from_secs(5))
    }

    #[cfg(unix)]
    #[test]
    fn should_treat_a_commit_github_has_never_seen_as_an_answer_not_a_failure() {
        let body = r#"{"message":"No commit found for SHA: abc","documentation_url":"x","status":"422"}"#;
        assert_eq!(
            replying(&format!("printf '%s' '{body}'; exit 1")),
            Ok(Reply::UnknownCommit)
        );
    }

    #[cfg(unix)]
    #[test]
    fn should_still_fail_on_other_api_errors() {
        for body in [
            r#"{"message":"Bad credentials","status":"401"}"#,
            r#"{"message":"Not Found","status":"404"}"#,
            r#"{"message":"Validation Failed","status":"422"}"#,
            "not json",
        ] {
            let error = replying(&format!("printf '%s' '{body}'; exit 1")).unwrap_err();
            assert!(error.contains("authentication, API, or network"), "{body}: {error}");
        }
    }

    #[test]
    fn should_report_a_merged_pull_request_whose_head_is_not_the_checkout() {
        let pull = |number: u64, sha: &str, base: &str| {
            serde_json::json!({
                "number": number, "head": {"sha": sha}, "merged_at": "2026-10-14T00:00:00Z",
                "base": {"repo": {"full_name": base}}
            })
        };
        let head = "0123456789012345678901234567890123456789";
        let answer = |pulls: serde_json::Value| classify(pulls.to_string().as_bytes(), "Goldziher/voom", head).unwrap();

        assert_eq!(
            answer(serde_json::json!([pull(7, "other", "Goldziher/voom")])),
            Hosted::HeadDiffers { pull: 7 }
        );
        assert_eq!(
            answer(serde_json::json!([
                pull(7, "other", "Goldziher/voom"),
                pull(9, head, "Goldziher/voom")
            ])),
            Hosted::Merged,
            "an exact head beats a differing one"
        );
        assert_eq!(
            answer(serde_json::json!([pull(7, "other", "someone/voom")])),
            Hosted::NotMerged
        );
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
