//! Detached removal through the real binary: a run moves artifacts into the trash and `voom reap`
//! frees them (ADR 0020).

// Helper functions here are not `#[test]` bodies, which is all `allow-expect-in-tests` covers.
// In a test crate a panic is the failure report, which is exactly what these want.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn voom(trash: &Path) -> Command {
    let mut command = Command::cargo_bin("voom").expect("the binary builds");
    command
        .env(voom::bazel::ROOTS_ENV, "")
        .env(voom::delete::TRASH_ENV, trash)
        .env_remove("VOOM_WAIT");
    command
}

fn project(root: &Path) {
    fs::create_dir_all(root.join("app/src")).unwrap();
    fs::write(root.join("app/Cargo.toml"), b"[package]\nname = \"app\"\n").unwrap();
    fs::write(root.join("app/src/main.rs"), b"fn main() {}").unwrap();
    fs::create_dir_all(root.join("app/target/debug/deps")).unwrap();
    fs::write(root.join("app/target/debug/deps/app.o"), vec![7u8; 64 * 1024]).unwrap();
}

fn pending(trash: &Path) -> usize {
    fs::read_dir(trash).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .count()
    })
}

#[test]
fn should_detach_an_artifact_and_free_it_on_reap() {
    let tree = TempDir::new().unwrap();
    let trash = TempDir::new().unwrap();
    project(tree.path());

    voom(trash.path())
        .arg(tree.path())
        .assert()
        .success()
        .stdout(contains("detached"));

    assert!(
        !tree.path().join("app/target").exists(),
        "the artifact has left its tree"
    );
    assert!(tree.path().join("app/src/main.rs").exists(), "the source stays");

    voom(trash.path())
        .arg("reap")
        .assert()
        .success()
        .stdout(contains("freed"));
    assert_eq!(pending(trash.path()), 0, "the reap leaves nothing behind");
}

#[test]
fn should_unlink_in_place_under_wait() {
    let tree = TempDir::new().unwrap();
    let trash = TempDir::new().unwrap();
    project(tree.path());

    voom(trash.path())
        .arg("--wait")
        .arg(tree.path())
        .assert()
        .success()
        .stdout(contains("removed"));

    assert!(!tree.path().join("app/target").exists());
    assert_eq!(pending(trash.path()), 0, "nothing went through the trash");
}

#[test]
fn should_detach_nothing_on_a_dry_run() {
    let tree = TempDir::new().unwrap();
    let trash = TempDir::new().unwrap();
    project(tree.path());

    voom(trash.path()).arg("-n").arg(tree.path()).assert().success();

    assert!(tree.path().join("app/target/debug/deps/app.o").exists());
    assert_eq!(pending(trash.path()), 0);
}
