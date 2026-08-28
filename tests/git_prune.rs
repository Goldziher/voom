//! Git housekeeping, against real repositories.
//!
//! Every fixture is a repository built with `git init` under a `TempDir` — the unit under test is
//! "what does voom do to somebody's repository", and nothing smaller than a real one is
//! convincing. Each sets its own `user.name` and `user.email`, and points `HEAD` at an explicit
//! branch, so the suite depends neither on the machine's git configuration nor on which default
//! branch this git version picked.
//!
//! If `git` is not on `PATH` the whole suite skips cleanly rather than failing: voom delegates to
//! git, and a machine without git is a machine where this feature correctly does nothing.

// A panic in fixture construction *is* the failure report. `allow-expect-in-tests` in
// clippy.toml only covers `#[test]` bodies, not the helpers below them.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use predicates::prelude::PredicateBooleanExt;
use support::snapshot;
use tempfile::TempDir;
use voom::git::{GitPruneOptions, Status, StepKind, prune, prune_during_sweep, prune_repositories};

/// Whether this machine has a git to delegate to.
fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Runs git in `dir`, returning its combined output. Panics with git's own complaint on failure,
/// because a broken fixture is a broken test and not a finding.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("running `git {}` in {}: {error}", args.join(" "), dir.display()));
    assert!(
        output.status.success(),
        "`git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository with one commit, at `root/name`.
fn repository(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).expect("the repository directory");
    git(&path, &["init", "--quiet"]);
    // Pinned locally so neither the machine's identity nor its git version's default branch can
    // change what these tests see.
    git(&path, &["config", "user.name", "voom tests"]);
    git(&path, &["config", "user.email", "tests@voom.invalid"]);
    git(&path, &["config", "commit.gpgsign", "false"]);
    git(&path, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    std::fs::write(path.join("README.md"), b"fixture").expect("a file to commit");
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "--quiet", "-m", "initial"]);
    path
}

/// The options a sweep uses: both local steps, no network, no object census.
fn sweep_options(root: &Path) -> GitPruneOptions {
    GitPruneOptions {
        roots: vec![root.to_path_buf()],
        jobs: Some(2),
        ..GitPruneOptions::default()
    }
}

/// The options `voom git-prune` uses: the same steps, plus the census under `--dry-run`.
fn subcommand_options(root: &Path) -> GitPruneOptions {
    GitPruneOptions {
        count_objects: true,
        ..sweep_options(root)
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().expect("the path exists")
}

/// A stale worktree is one whose checkout is gone; git keeps administrative files for it under
/// `.git/worktrees/<name>` until something prunes them. A *live* one must survive, because
/// removing its administrative files would detach a checkout the user is working in.
#[test]
fn should_prune_a_stale_worktree_and_leave_a_live_one() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let repository = repository(root.path(), "api");
    git(&repository, &["worktree", "add", "--quiet", "../live", "-b", "live"]);
    git(&repository, &["worktree", "add", "--quiet", "../stale", "-b", "stale"]);
    std::fs::remove_dir_all(root.path().join("stale")).expect("the stale checkout goes away");

    let result = prune(&subcommand_options(root.path())).expect("git is available");

    let stale = repository.join(".git/worktrees/stale");
    let live = repository.join(".git/worktrees/live");
    let reported = result.repositories[0]
        .steps
        .iter()
        .find(|step| step.kind == StepKind::WorktreePrune)
        .and_then(|step| step.summary().map(ToOwned::to_owned))
        .unwrap_or_default();
    assert_eq!(
        reported,
        "1 stale worktree pruned",
        "the report must say what was removed from {}",
        repository.display()
    );
    assert!(
        !stale.exists(),
        "the stale worktree's administrative files remain: {}",
        stale.display()
    );
    assert!(live.exists(), "a live worktree must not be pruned: {}", live.display());
    assert!(
        root.path().join("live").exists(),
        "and its checkout must still be there: {}",
        root.path().join("live").display()
    );
}

/// Running a repack under somebody's paused rebase is the avoidable harm this rail exists for.
/// The marker directory is exactly what git itself writes, and exactly what the rail reads.
#[test]
fn should_skip_a_repository_mid_rebase_and_name_it() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let repository = repository(root.path(), "halfway");
    std::fs::create_dir_all(repository.join(".git/rebase-merge")).expect("the rebase state");
    std::fs::write(repository.join(".git/rebase-merge/head-name"), b"refs/heads/main").expect("its head-name");

    let result = prune(&subcommand_options(root.path())).expect("git is available");

    let entry = result
        .repositories
        .iter()
        .find(|found| found.path == canonical(&repository))
        .unwrap_or_else(|| panic!("{} was not reported at all", repository.display()));
    assert_eq!(
        entry.status(),
        Status::Skipped,
        "{} was not skipped: {:?}",
        repository.display(),
        entry.steps
    );
    assert!(entry.steps.is_empty(), "nothing may run in a repository mid-rebase");
    let reason = entry.skipped.as_ref().map(ToString::to_string).unwrap_or_default();
    assert!(
        reason.contains("rebase-merge"),
        "the reason must name the marker, not just say `skipped`: {reason}"
    );
}

/// Four worktrees of one repository share one object store. Treating each as a repository of its
/// own would run the same housekeeping four times over the same objects.
#[test]
fn should_prune_a_repository_with_linked_worktrees_exactly_once() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let repository = repository(root.path(), "api");
    git(&repository, &["worktree", "add", "--quiet", "../one", "-b", "one"]);
    git(&repository, &["worktree", "add", "--quiet", "../two", "-b", "two"]);

    let result = prune(&subcommand_options(root.path())).expect("git is available");

    let paths: Vec<&Path> = result.repositories.iter().map(|found| found.path.as_path()).collect();
    assert_eq!(
        paths,
        vec![canonical(&repository).as_path()],
        "a linked worktree is not a repository of its own; got {paths:?}"
    );
}

/// `--dry-run` is the same pipeline with the repack withheld, so its report has to be what a real
/// run then does — otherwise it is a gesture rather than an audit step.
#[test]
fn should_change_nothing_under_dry_run_and_report_what_a_real_run_then_does() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let repository = repository(root.path(), "api");
    git(&repository, &["worktree", "add", "--quiet", "../stale", "-b", "stale"]);
    std::fs::remove_dir_all(root.path().join("stale")).expect("the stale checkout goes away");
    let before = snapshot(root.path());

    let dry = prune(&GitPruneOptions {
        dry_run: true,
        ..subcommand_options(root.path())
    })
    .expect("git is available");

    assert_eq!(snapshot(root.path()), before, "a dry run left the tree different");
    let predicted = dry.repositories[0]
        .steps
        .iter()
        .find(|step| step.kind == StepKind::WorktreePrune)
        .and_then(|step| step.summary().map(ToOwned::to_owned))
        .unwrap_or_default();
    assert_eq!(
        predicted, "1 stale worktree pruned",
        "the dry run did not say what it would do"
    );

    let real = prune(&subcommand_options(root.path())).expect("git is available");

    let done = real.repositories[0]
        .steps
        .iter()
        .find(|step| step.kind == StepKind::WorktreePrune)
        .and_then(|step| step.summary().map(ToOwned::to_owned))
        .unwrap_or_default();
    assert_eq!(
        done, predicted,
        "the real run did something the dry run did not predict"
    );
    assert!(
        !repository.join(".git/worktrees/stale").exists(),
        "and the real run really removed it"
    );
}

#[test]
fn should_ignore_a_directory_that_is_not_a_repository() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    std::fs::create_dir_all(root.path().join("just/a/tree")).expect("a plain tree");
    std::fs::write(root.path().join("just/a/tree/notes.txt"), b"nothing to see").expect("a file");
    let before = snapshot(root.path());

    let result = prune(&subcommand_options(root.path())).expect("git is available");

    assert!(
        result.repositories.is_empty(),
        "a plain directory was taken for a repository: {:?}",
        result.repositories
    );
    assert_eq!(snapshot(root.path()), before);
}

/// One repository's failure is reported and never aborts the run — the same isolation the
/// deleter gives one artifact's failure.
#[test]
fn should_report_a_failing_repository_without_aborting_the_run() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let broken = repository(root.path(), "broken");
    let healthy = repository(root.path(), "healthy");
    // A config git cannot parse makes every command in this repository fail, which is a real
    // failure rather than a simulated one.
    std::fs::write(broken.join(".git/config"), b"[core\n").expect("a broken config");

    let result = prune(&subcommand_options(root.path())).expect("git is available");

    let status = |path: &Path| {
        result
            .repositories
            .iter()
            .find(|found| found.path == canonical(path))
            .unwrap_or_else(|| panic!("{} was not reported", path.display()))
            .status()
    };
    assert_eq!(
        status(&broken),
        Status::Failed,
        "{} should have failed",
        broken.display()
    );
    assert_eq!(
        status(&healthy),
        Status::Pruned,
        "{} must be pruned despite its neighbour failing",
        healthy.display()
    );
    assert_eq!(result.totals().failed, 1);
    assert_eq!(result.exit_code(), 1, "a failed repository is a non-zero exit");
}

/// The default path is strictly local. Asserted on the argv voom constructed rather than on
/// behaviour, so proving the absence of a network call does not need a network.
#[test]
fn should_run_no_networked_command_during_a_default_sweep() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let repository = repository(root.path(), "api");
    git(
        &repository,
        &["remote", "add", "origin", "https://example.invalid/api.git"],
    );

    let result = prune_during_sweep(std::slice::from_ref(&repository), &sweep_options(root.path())).expect("a result");

    let commands: Vec<&[String]> = result.repositories[0]
        .steps
        .iter()
        .map(|step| step.command.as_slice())
        .collect();
    assert!(
        !commands.iter().any(|command| command.iter().any(|arg| arg == "remote")),
        "a sweep built a command that would contact the remote: {commands:?}"
    );
    assert_eq!(
        commands,
        vec![
            &["git", "worktree", "prune", "-v"].map(ToOwned::to_owned)[..],
            &["git", "gc", "--auto"].map(ToOwned::to_owned)[..],
        ]
    );
}

/// A repository below git's own repacking thresholds is left alone, because the step voom runs
/// is the one git itself runs after every commit. Asserted on the command chosen rather than on
/// timing, which would be a flake.
#[test]
fn should_repack_only_on_gits_own_thresholds() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let repository = repository(root.path(), "api");

    let result = prune_repositories(&[repository], &sweep_options(root.path())).expect("git is available");

    let repack = result.repositories[0]
        .steps
        .iter()
        .find(|step| step.kind == StepKind::Gc)
        .expect("a repack step");
    assert_eq!(repack.command, vec!["git", "gc", "--auto"], "never a bare `gc`");
    assert!(repack.succeeded(), "{:?}", repack.outcome);
    assert_eq!(
        repack.summary(),
        None,
        "a repository below git's thresholds has nothing to report, which is what keeps a sweep silent"
    );
}

/// A sweep that reclaimed nothing from git prints nothing about git. This is the contract the
/// sweep's report depends on, so it is asserted on the result rather than left to the renderer.
#[test]
fn should_have_nothing_noteworthy_to_say_about_a_quiet_repository() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let repository = repository(root.path(), "api");

    let result = prune_during_sweep(&[repository], &sweep_options(root.path())).expect("a result");

    assert!(
        result.noteworthy().is_empty(),
        "a quiet repository would have produced output: {:?}",
        result.repositories
    );
}

/// The sweep's entry point never fails a run that reclaimed disk, and never spawns git for a tree
/// that held no repositories.
#[test]
fn should_say_nothing_when_a_sweep_walked_past_no_repositories() {
    let root = TempDir::new().expect("a temporary directory");
    assert!(prune_during_sweep(&[], &sweep_options(root.path())).is_none());
}

/// A repository the walker passed is pruned by the sweep's entry point without anyone asking —
/// the call `run.rs` makes, with the paths `scan` collects.
#[test]
fn should_prune_a_repository_a_sweep_walked_past() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let repository = repository(root.path(), "api");
    git(&repository, &["worktree", "add", "--quiet", "../stale", "-b", "stale"]);
    std::fs::remove_dir_all(root.path().join("stale")).expect("the stale checkout goes away");

    let result = prune_during_sweep(std::slice::from_ref(&repository), &sweep_options(root.path())).expect("a result");

    assert!(
        !repository.join(".git/worktrees/stale").exists(),
        "the sweep did not prune the stale worktree it walked past"
    );
    assert_eq!(result.noteworthy().len(), 1, "and it is worth a line in the report");
}

/// A repository with one stale worktree and one Rust artifact beside it, for the wiring tests
/// below. Returns the tree to sweep and the administrative directory that must or must not
/// survive it.
fn tree_with_a_stale_worktree(root: &Path) -> PathBuf {
    let repository = repository(root, "project");
    git(&repository, &["worktree", "add", "--quiet", "../stale", "-b", "stale"]);
    std::fs::remove_dir_all(root.join("stale")).expect("removing the checkout makes it stale");

    std::fs::create_dir_all(repository.join("target")).expect("an artifact directory");
    std::fs::write(repository.join("Cargo.toml"), b"[package]").expect("a marker");
    std::fs::write(repository.join("target/app"), vec![0u8; 4096]).expect("build output");

    repository.join(".git/worktrees/stale")
}

fn voom() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("voom").expect("the binary builds")
}

/// The wiring, end to end through the real binary: an ordinary sweep prunes what git considers
/// prunable in the repositories it walks past, without being asked to.
///
/// This is the behaviour that makes housekeeping *default*, and it is the one thing the git
/// module cannot test on its own — discovery happens in the artifact walker.
#[test]
fn should_prune_a_repository_a_sweep_walked_past_without_being_asked() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let stale = tree_with_a_stale_worktree(root.path());

    voom()
        .arg(root.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("1 stale worktree pruned"));

    assert!(
        !stale.exists(),
        "an ordinary sweep prunes it: {} is still there",
        stale.display()
    );
    assert!(
        !root.path().join("project/target").exists(),
        "and it still removed the build artifact it was actually run for"
    );
}

/// `--no-git` turns it off, and the report then says nothing about git at all.
#[test]
fn should_not_touch_a_repository_when_git_housekeeping_is_turned_off() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let stale = tree_with_a_stale_worktree(root.path());

    voom()
        .args(["--no-git", "--color", "never"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("worktree").not())
        .stdout(predicates::str::contains(" · git ").not());

    assert!(
        stale.exists(),
        "--no-git leaves the repository alone: {} was pruned anyway",
        stale.display()
    );
}

/// The same, from `voom.toml`, because a flag nobody can commit is a flag a repository cannot
/// set a policy with (ADR 0004).
#[test]
fn should_honour_a_config_file_turning_git_housekeeping_off() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let stale = tree_with_a_stale_worktree(root.path());
    std::fs::write(root.path().join("voom.toml"), b"[git]\nenabled = false\n").expect("a config file");

    voom().arg(root.path()).assert().success();

    assert!(
        stale.exists(),
        "`[git] enabled = false` leaves the repository alone: {} was pruned anyway",
        stale.display()
    );
}

/// A dry run reports what housekeeping would do and does none of it — the same rule the artifact
/// half follows, and the one that makes `--dry-run` safe to run anywhere.
#[test]
fn should_predict_git_housekeeping_in_a_dry_run_without_performing_it() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let stale = tree_with_a_stale_worktree(root.path());
    let before = snapshot(root.path());

    voom()
        .args(["--dry-run", "--color", "never"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("would prune"))
        .stdout(predicates::str::contains("1 stale worktree pruned"));

    assert!(stale.exists(), "a dry run prunes nothing: {}", stale.display());
    assert_eq!(snapshot(root.path()), before, "and leaves the tree byte-identical");
}

/// A repository with nothing to do produces no git output at all. Housekeeping runs in nearly
/// every repository a sweep passes and nearly always finds nothing, so a block that appeared
/// whenever it *ran* would be on almost every report.
#[test]
fn should_say_nothing_about_a_repository_that_needed_nothing() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let repository = repository(root.path(), "project");
    std::fs::create_dir_all(repository.join("target")).expect("an artifact directory");
    std::fs::write(repository.join("Cargo.toml"), b"[package]").expect("a marker");
    std::fs::write(repository.join("target/app"), vec![0u8; 4096]).expect("build output");

    voom()
        .args(["--color", "never"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("pruned").not());
}

/// The flag beats the file, on the same principle as every other option: no `voom.toml` below a
/// scan root may turn back on something the invocation switched off.
#[test]
fn should_let_the_flag_beat_a_config_file_that_turns_git_housekeeping_on() {
    if !git_available() {
        return;
    }
    let root = TempDir::new().expect("a temporary directory");
    let stale = tree_with_a_stale_worktree(root.path());
    std::fs::write(root.path().join("voom.toml"), b"[git]\nenabled = true\n").expect("a config file");

    voom().args(["--no-git"]).arg(root.path()).assert().success();

    assert!(
        stale.exists(),
        "--no-git wins over `[git] enabled = true`: {} was pruned anyway",
        stale.display()
    );
}
