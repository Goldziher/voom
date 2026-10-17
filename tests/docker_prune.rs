//! Docker housekeeping, against a fake `docker` on `PATH`.
//!
//! Nothing here talks to a real daemon. A shell script named `docker` (and one named `colima`)
//! stands in for each, answers from recorded JSON, and appends every invocation to a log — so a
//! test can assert exactly which commands ran, and, as importantly, which did not. Behaviour is
//! steered by environment variables the script reads, which keeps the script itself the same
//! for every test.
//!
//! Unix only: the fakes are `sh` scripts, and the VM trim they stand in for is a unix concern.

#![cfg(unix)]
// A panic in fixture construction *is* the failure report. `allow-expect-in-tests` in
// clippy.toml only covers `#[test]` bodies, not the helpers below them.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;
use tempfile::TempDir;

/// Answers the calls voom makes, from `$FAKE_DF`, and fails the way `$FAKE_FAIL` says to.
const FAKE_DOCKER: &str = r#"#!/bin/sh
echo "docker $*" >> "$FAKE_LOG"
case "$FAKE_FAIL" in
  down) echo "Cannot connect to the Docker daemon at unix:///x.sock. Is the docker daemon running?" >&2; exit 1 ;;
  denied) echo "permission denied while trying to connect to the Docker daemon socket" >&2; exit 1 ;;
esac
case "$1 $2" in
  "version --format") echo "${FAKE_API:-1.54}" ;;
  "system df") cat "$FAKE_DF" ;;
  "context inspect") echo "$FAKE_ENDPOINT" ;;
  "image prune") echo "Total reclaimed space: 1.5GB" ;;
  "builder prune") printf 'ID\tRECLAIMABLE\tSIZE\nTotal:\t2GB\n' ;;
  "volume prune") echo "Total reclaimed space: 30MB" ;;
  *) echo "unexpected: $*" >&2; exit 64 ;;
esac
"#;

/// Stands in for the in-VM trim by shrinking the "disk image" to one block.
const FAKE_COLIMA: &str = r#"#!/bin/sh
echo "colima $*" >> "$FAKE_LOG"
if [ -n "$FAKE_DISK" ]; then printf x > "$FAKE_DISK"; fi
echo "/mnt/lima-colima: 38 GiB trimmed"
"#;

const COLIMA_SOCKET: &str = "unix:///home/dev/.colima/default/docker.sock";

/// The directory holding the fakes, built once so no test executes a script another is still
/// writing.
fn fake_bin() -> &'static Path {
    static BIN: OnceLock<TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let dir = TempDir::new().expect("a temporary directory");
        for (name, body) in [("docker", FAKE_DOCKER), ("colima", FAKE_COLIMA)] {
            let path = dir.path().join(name);
            fs::write(&path, body).expect("writes the fake");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("marks it executable");
        }
        dir
    })
    .path()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/docker")
        .join(name)
}

/// One test's sandbox: its own call log, home, and host "disk image".
struct Sandbox {
    dir: TempDir,
    disk: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = TempDir::new().expect("a temporary directory");
        let disk = dir.path().join("datadisk");
        // Nonzero bytes, so the filesystem really allocates them (zeros could be stored sparse).
        fs::write(&disk, vec![7u8; 1 << 20]).expect("writes the disk image");
        Self { dir, disk }
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default()
    }

    fn voom(&self) -> Command {
        let mut command = Command::cargo_bin("voom").expect("the binary builds");
        let path = format!("{}:/usr/bin:/bin", fake_bin().display());
        command
            .env("PATH", path)
            .env("HOME", self.dir.path())
            .env(voom::bazel::ROOTS_ENV, "")
            .env(voom::docker::DISK_IMAGES_ENV, &self.disk)
            .env("DOCKER_HOST", COLIMA_SOCKET)
            .env("FAKE_LOG", self.dir.path().join("calls.log"))
            .env("FAKE_DF", fixture("df_stable.json"))
            .env("FAKE_DISK", &self.disk)
            .env_remove("FAKE_FAIL");
        command
    }

    fn allocated(&self) -> u64 {
        voom::docker::allocated_bytes(std::slice::from_ref(&self.disk)).expect("the disk image exists")
    }
}

fn json(command: &mut Command) -> Value {
    let output = command.output().expect("the binary runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON document")
}

fn lane<'a>(document: &'a Value, kind: &str) -> &'a Value {
    document["lanes"]
        .as_array()
        .and_then(|lanes| lanes.iter().find(|lane| lane["kind"] == kind))
        .unwrap_or_else(|| panic!("no `{kind}` lane in {document}"))
}

#[test]
fn should_plan_without_pruning_or_trimming_on_a_dry_run() {
    let sandbox = Sandbox::new();
    let before = sandbox.allocated();

    let document = json(sandbox.voom().args(["docker-prune", "-n", "--format", "json"]));

    assert_eq!(document["dry_run"], true);
    assert_eq!(document["backend"], "colima (profile default)");
    // The old unused image and the dangling one; not the one a container uses, nor the young one.
    assert_eq!(lane(&document, "images")["candidates"], 2);
    assert_eq!(lane(&document, "images")["estimated_bytes"], 1_100_000_000_u64);
    assert_eq!(lane(&document, "images")["outcome"], "would_reclaim");
    assert_eq!(lane(&document, "build_cache")["estimated_bytes"], 500_000_000_u64);
    assert_eq!(document["trim"]["outcome"], "would_trim");
    assert_eq!(document["trim"]["host_bytes_before"], before);

    let log = sandbox.log();
    assert!(!log.contains("prune"), "a dry run must not prune: {log}");
    assert!(!log.contains("colima"), "a dry run must not trim: {log}");
    assert_eq!(sandbox.allocated(), before, "the disk image is untouched");
}

#[test]
fn should_prune_with_filters_and_report_dockers_own_bytes_then_trim() {
    let sandbox = Sandbox::new();
    let before = sandbox.allocated();

    let document = json(sandbox.voom().args(["docker-prune", "--format", "json"]));

    assert_eq!(
        lane(&document, "images")["reclaimed_bytes"],
        3_000_000_000_u64,
        "two prunes of 1.5GB"
    );
    assert_eq!(lane(&document, "build_cache")["reclaimed_bytes"], 2_000_000_000_u64);
    assert_eq!(document["totals"]["bytes"], 5_000_000_000_u64);

    let log = sandbox.log();
    for expected in [
        "docker image prune --force\n",
        "docker image prune --all --force --filter until=168h\n",
        "docker builder prune --all --force --filter until=168h\n",
        "colima ssh -p default -- sudo fstrim -av\n",
    ] {
        assert!(log.contains(expected), "missing `{expected}` in {log}");
    }
    for forbidden in ["container", "volume", "system prune", "rm "] {
        assert!(!log.contains(forbidden), "`{forbidden}` must never run: {log}");
    }

    assert_eq!(document["trim"]["outcome"], "trimmed");
    assert_eq!(document["trim"]["host_bytes_before"], before);
    let returned = document["trim"]["host_bytes_returned"].as_u64().expect("measured");
    assert!(returned > 0 && returned == before - sandbox.allocated(), "{document}");
}

#[test]
fn should_map_max_age_to_hours_and_skip_the_trim_on_request() {
    let sandbox = Sandbox::new();

    let document = json(
        sandbox
            .voom()
            .args(["docker-prune", "--max-age", "36h", "--no-trim", "--format", "json"]),
    );

    assert!(sandbox.log().contains("--filter until=36h"), "{}", sandbox.log());
    assert!(!sandbox.log().contains("colima"));
    assert_eq!(document["trim"], Value::Null);
}

#[test]
fn should_leave_volumes_alone_unless_asked() {
    let sandbox = Sandbox::new();
    sandbox.voom().args(["docker-prune"]).assert().success();
    assert!(!sandbox.log().contains("volume prune"), "{}", sandbox.log());
}

#[test]
fn should_remove_only_anonymous_volumes_for_a_bare_volumes_flag_and_all_on_request() {
    let sandbox = Sandbox::new();
    let anonymous = json(
        sandbox
            .voom()
            .args(["docker-prune", "-n", "--volumes", "--format", "json"]),
    );
    assert_eq!(lane(&anonymous, "volumes")["candidates"], 1);
    assert_eq!(
        lane(&anonymous, "volumes")["commands"][0],
        "docker volume prune --force"
    );

    let everything = json(
        sandbox
            .voom()
            .args(["docker-prune", "-n", "--volumes-all", "--format", "json"]),
    );
    assert_eq!(lane(&everything, "volumes")["candidates"], 2, "the mounted one stays");
    assert_eq!(
        lane(&everything, "volumes")["commands"][0],
        "docker volume prune --all --force"
    );
}

#[test]
fn should_refuse_anonymous_pruning_on_a_daemon_too_old_to_honour_it() {
    let sandbox = Sandbox::new();
    let document = json(
        sandbox
            .voom()
            .env("FAKE_API", "1.41")
            .args(["docker-prune", "--volumes", "--format", "json"]),
    );
    assert_eq!(lane(&document, "volumes")["outcome"], "skipped");
    assert!(!sandbox.log().contains("volume prune"), "{}", sandbox.log());
}

#[test]
fn should_read_the_endpoint_from_the_current_context_when_docker_host_is_unset() {
    let sandbox = Sandbox::new();
    let document = json(
        sandbox
            .voom()
            .env_remove("DOCKER_HOST")
            .env("FAKE_ENDPOINT", "unix:///home/dev/.colima/work/docker.sock")
            .args(["docker-prune", "-n", "--format", "json"]),
    );
    assert_eq!(document["backend"], "colima (profile work)");
    assert_eq!(document["trim"]["command"], "colima ssh -p work -- sudo fstrim -av");
}

#[test]
fn should_not_trim_a_backend_that_trims_itself() {
    let sandbox = Sandbox::new();
    let document = json(
        sandbox
            .voom()
            .env("DOCKER_HOST", "unix:///home/dev/.orbstack/run/docker.sock")
            .args(["docker-prune", "--format", "json"]),
    );
    assert_eq!(document["backend"], "orbstack");
    assert_eq!(document["trim"], Value::Null);
    assert!(!sandbox.log().contains("colima"));
}

#[test]
fn should_skip_with_one_line_and_exit_zero_when_the_daemon_is_down() {
    let sandbox = Sandbox::new();
    sandbox
        .voom()
        .env("FAKE_FAIL", "down")
        .arg("docker-prune")
        .assert()
        .success()
        .stdout("voom: docker: the docker daemon is not running (skipped)\n");
}

#[test]
fn should_skip_on_a_permission_refusal() {
    let sandbox = Sandbox::new();
    sandbox
        .voom()
        .env("FAKE_FAIL", "denied")
        .arg("docker-prune")
        .assert()
        .success()
        .stdout(contains("permission denied"));
}

#[test]
fn should_skip_when_docker_is_not_installed() {
    let sandbox = Sandbox::new();
    let empty = TempDir::new().expect("a temporary directory");
    let document = json(
        sandbox
            .voom()
            .env("PATH", empty.path())
            .args(["docker-prune", "--format", "json"]),
    );
    assert_eq!(document["status"], "skipped");
    assert_eq!(document["skipped_reason"], "`docker` is not installed");
}

fn tree_with_one_artifact() -> TempDir {
    let root = TempDir::new().expect("a temporary directory");
    fs::create_dir_all(root.path().join("proj/target/debug")).unwrap();
    fs::write(root.path().join("proj/Cargo.toml"), b"[package]").unwrap();
    fs::write(root.path().join("proj/target/debug/app"), vec![0u8; 4096]).unwrap();
    root
}

#[test]
fn should_not_talk_to_docker_during_a_sweep_unless_asked() {
    let sandbox = Sandbox::new();
    let tree = tree_with_one_artifact();

    sandbox
        .voom()
        .args(["-n", "--no-bazel", "--no-git"])
        .arg(tree.path())
        .assert()
        .success();

    assert_eq!(sandbox.log(), "", "docker must not be invoked by default");
}

#[test]
fn should_prune_docker_beside_a_sweep_and_keep_json_stdout_a_single_document() {
    let sandbox = Sandbox::new();
    let tree = tree_with_one_artifact();

    let output = sandbox
        .voom()
        .args(["-n", "--no-bazel", "--no-git", "--docker", "--format", "json"])
        .arg(tree.path())
        .output()
        .expect("the binary runs");

    assert!(output.status.success());
    let document: Value = serde_json::from_slice(&output.stdout).expect("stdout is exactly one document");
    assert!(document["schema_version"].is_number());
    assert!(
        document["docker"].is_object(),
        "the docker stage is part of the document: {document}"
    );
    assert!(document["bazel"].is_null(), "--no-bazel leaves the bazel stage out");
    assert!(
        !sandbox.log().contains("prune"),
        "-n withholds the prunes: {}",
        sandbox.log()
    );
}

#[test]
fn should_print_the_docker_section_after_a_human_sweep() {
    let sandbox = Sandbox::new();
    let tree = tree_with_one_artifact();

    sandbox
        .voom()
        .args(["-n", "--no-bazel", "--no-git", "--docker"])
        .arg(tree.path())
        .assert()
        .success()
        .stdout(contains("docker — colima (profile default)"))
        .stdout(contains("would reclaim"));
}

#[test]
fn should_note_a_down_daemon_on_stderr_and_still_pass_the_sweep() {
    let sandbox = Sandbox::new();
    let tree = tree_with_one_artifact();

    sandbox
        .voom()
        .env("FAKE_FAIL", "down")
        .args(["-n", "--no-bazel", "--no-git", "--docker"])
        .arg(tree.path())
        .assert()
        .success()
        .stderr(contains("voom: docker: the docker daemon is not running (skipped)"));
}

#[test]
fn should_pass_the_sweep_age_and_volume_scope_to_docker() {
    let sandbox = Sandbox::new();
    let tree = tree_with_one_artifact();

    sandbox
        .voom()
        .args([
            "--no-bazel",
            "--no-git",
            "--docker",
            "--docker-max-age",
            "3d",
            "--docker-volumes",
        ])
        .arg(tree.path())
        .assert()
        .success();

    let log = sandbox.log();
    assert!(log.contains("--filter until=72h"), "{log}");
    assert!(log.contains("docker volume prune --force\n"), "{log}");
}

#[test]
fn should_require_docker_for_docker_volumes() {
    let sandbox = Sandbox::new();
    sandbox
        .voom()
        .args(["--docker-volumes", "."])
        .assert()
        .failure()
        .code(2);
    assert_eq!(sandbox.log(), "");
}

#[test]
fn should_enable_docker_from_the_user_config() {
    let sandbox = Sandbox::new();
    let tree = tree_with_one_artifact();
    let config = sandbox.dir.path().join("machine.toml");
    fs::write(&config, "[docker]\nenabled = true\nmax_age = \"2d\"\n").unwrap();

    sandbox
        .voom()
        .args(["--no-bazel", "--no-git", "--config"])
        .arg(&config)
        .arg(tree.path())
        .assert()
        .success();

    assert!(sandbox.log().contains("--filter until=48h"), "{}", sandbox.log());
}
