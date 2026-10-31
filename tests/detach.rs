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

#[test]
fn should_list_the_trash_without_freeing_it() {
    let trash = TempDir::new().unwrap();
    let entry = trash.path().join("1793431468123952000-68131-2-p-target");
    fs::create_dir_all(&entry).unwrap();
    fs::write(entry.join("blob.bin"), vec![3u8; 4096]).unwrap();

    voom(trash.path())
        .arg("reap")
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(contains("in the trash"));

    assert!(entry.exists(), "a dry run frees nothing");

    voom(trash.path())
        .arg("reap")
        .assert()
        .success()
        .stdout(contains("freed"));
    assert_eq!(pending(trash.path()), 0);
}

#[test]
fn should_free_an_earlier_runs_leftovers_without_being_asked() {
    let trash = TempDir::new().unwrap();
    let entry = trash.path().join("1793388769703569000-13306-0-p-poly");
    fs::create_dir_all(&entry).unwrap();
    fs::write(entry.join("blob.bin"), vec![5u8; 4096]).unwrap();
    let empty = TempDir::new().unwrap();

    voom(trash.path()).arg(empty.path()).assert().success();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while pending(trash.path()) != 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        pending(trash.path()),
        0,
        "a real run drains what an interrupted earlier run left in the trash"
    );
}

#[cfg(unix)]
#[test]
fn should_free_an_entry_whose_directory_kept_a_read_only_bit() {
    use std::os::unix::fs::PermissionsExt;

    let trash = TempDir::new().unwrap();
    let entry = trash.path().join("1793431468123952000-68131-2-p-target");
    let locked = entry.join("debug");
    fs::create_dir_all(&locked).unwrap();
    fs::write(locked.join("app.o"), vec![1u8; 1024]).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();

    voom(trash.path())
        .arg("reap")
        .assert()
        .success()
        .stdout(contains("freed"));

    assert_eq!(
        pending(trash.path()),
        0,
        "a read-only directory inside an entry does not make it immortal"
    );
}
