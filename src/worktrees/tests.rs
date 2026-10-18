mod aggressive;

use std::process::Command;

use super::*;

fn run(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

/// A repository on `main` with one commit touching `dist/app.js` and `src.txt`.
fn repository() -> (tempfile::TempDir, PathBuf) {
    let fixture = tempfile::tempdir().expect("a tempdir");
    let main = fixture.path().canonicalize().unwrap().join("main");
    std::fs::create_dir_all(main.join("dist")).unwrap();
    std::fs::write(main.join("dist/app.js"), "built").unwrap();
    std::fs::write(main.join("src.txt"), "source").unwrap();
    run(&main, &["init", "-q", "-b", "main"]);
    run(&main, &["add", "."]);
    run(&main, &["commit", "-q", "-m", "init"]);
    (fixture, main)
}

fn add_worktree(main: &Path, name: &str) -> PathBuf {
    let path = main.parent().unwrap().join(name);
    // git rejects the verbatim `\\?\` form `canonicalize` produces on Windows. The worktree is
    // still created at `path`, and the product canonicalizes what git reports back, so the two
    // spellings meet.
    let target = crate::git::for_git(&path).display().to_string();
    run(main, &["worktree", "add", "-q", "--detach", target.as_str()]);
    path
}

fn prune_real(main: &Path, dry_run: bool) -> WorktreePruneResult {
    let root = main.parent().expect("a parent").to_path_buf();
    prune(
        &[main.to_path_buf()],
        &[root],
        WorktreeOptions {
            dry_run,
            stale_after: None,
            github_merged: false,
            aggressive: false,
        },
    )
}

fn state_of<'a>(result: &'a WorktreePruneResult, path: &Path) -> &'a Worktree {
    result
        .repositories
        .iter()
        .flat_map(|repository| &repository.worktrees)
        .find(|worktree| worktree.path == path)
        .expect("the worktree is listed")
}

#[test]
fn should_remove_a_clean_merged_worktree_and_keep_its_branch() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "merged");

    let result = prune_real(&main, false);

    assert_eq!(state_of(&result, &path).outcome, Some(Outcome::Removed));
    assert!(!path.exists());
}

#[test]
fn should_only_report_under_a_dry_run() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "merged");

    let result = prune_real(&main, true);

    assert_eq!(state_of(&result, &path).outcome, Some(Outcome::WouldRemove));
    assert!(path.exists());
}

#[test]
fn should_ignore_standalone_repositories_without_a_default_branch() {
    let (_fixture, main) = repository();
    run(&main, &["branch", "-m", "review-snapshot"]);

    let result = prune_real(&main, true);

    assert_eq!(
        result.repositories.len(),
        0,
        "a standalone clone has no linked worktrees"
    );
    assert!(main.join("src.txt").exists());
}

#[test]
fn should_ignore_an_unborn_repository_without_linked_worktrees() {
    let fixture = tempfile::tempdir().unwrap();
    let main = fixture.path().canonicalize().unwrap();
    run(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("notes.txt"), "unsaved work").unwrap();

    let result = prune_real(&main, false);

    assert_eq!(
        result.repositories.len(),
        0,
        "an unborn repository has no linked worktrees"
    );
    assert_eq!(std::fs::read_to_string(main.join("notes.txt")).unwrap(), "unsaved work");
}

#[test]
fn should_report_a_missing_default_branch_when_linked_worktrees_need_it() {
    let (_fixture, main) = repository();
    run(&main, &["branch", "-m", "review-snapshot"]);
    let path = add_worktree(&main, "linked");

    let result = prune_real(&main, false);

    assert_eq!(
        result.repositories[0].error.as_deref(),
        Some("no default branch found (origin/HEAD, main, master)")
    );
    assert!(path.join("src.txt").exists());
}

#[test]
fn should_remove_a_reachable_stale_worktree_without_a_default_branch() {
    let (_fixture, main) = repository();
    run(&main, &["branch", "-m", "review-snapshot"]);
    let path = unmerged_worktree(&main, "old-work", 90);

    let result = prune_stale(&main, 30);

    assert_eq!(state_of(&result, &path).state, State::Stale { discarded: 0 });
    assert_eq!(state_of(&result, &path).outcome, Some(Outcome::Removed));
    assert!(!path.exists());
    assert!(git(&main, &["rev-parse", "--verify", "refs/heads/old-work"]).is_some());
}

#[test]
fn should_keep_local_changes_without_a_default_branch_when_stale_cleanup_is_requested() {
    let (_fixture, main) = repository();
    run(&main, &["branch", "-m", "review-snapshot"]);
    let path = unmerged_worktree(&main, "old-work", 90);
    std::fs::write(path.join("work.txt"), "unsaved work").unwrap();

    let result = prune_stale(&main, 30);

    assert_eq!(state_of(&result, &path).state, State::LocalChanges { changed: 1 });
    assert_eq!(std::fs::read_to_string(path.join("work.txt")).unwrap(), "unsaved work");
}

#[test]
fn should_discard_only_deleted_build_output() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "built");
    std::fs::remove_file(path.join("dist/app.js")).unwrap();

    let result = prune_real(&main, false);

    assert_eq!(state_of(&result, &path).state, State::Merged { discarded: 1 });
    assert!(!path.exists());
}

/// A deleted *tracked* file under a deeper directory that merely shares a build-output name is
/// source, not build output, and keeps the worktree.
#[test]
fn should_keep_a_deleted_tracked_source_file_under_a_nested_build_directory() {
    let (_fixture, main) = repository();
    std::fs::create_dir_all(main.join("src/build")).unwrap();
    std::fs::write(main.join("src/build/gen.rs"), "source").unwrap();
    run(&main, &["add", "src/build/gen.rs"]);
    run(&main, &["commit", "-q", "-m", "source"]);
    let path = add_worktree(&main, "nested");
    std::fs::remove_file(path.join("src/build/gen.rs")).unwrap();

    let result = prune_real(&main, false);

    assert!(matches!(state_of(&result, &path).state, State::LocalChanges { .. }));
    assert!(path.exists());
}

/// A monorepo commits `client/dist/` several levels down; its deletion is build output, not work.
#[test]
fn should_remove_a_merged_worktree_whose_only_change_is_a_deleted_nested_dist() {
    let (_fixture, main) = repository();
    std::fs::create_dir_all(main.join("app/client/dist")).unwrap();
    std::fs::write(main.join("app/client/dist/icons.svg"), "generated").unwrap();
    run(&main, &["add", "app/client/dist/icons.svg"]);
    run(&main, &["commit", "-q", "-m", "dist"]);
    let path = add_worktree(&main, "nested-dist");
    std::fs::remove_file(path.join("app/client/dist/icons.svg")).unwrap();

    let result = prune_real(&main, false);

    assert_eq!(state_of(&result, &path).state, State::Merged { discarded: 1 });
    assert!(!path.exists());
}

#[test]
fn should_keep_a_merged_worktree_with_a_modified_file() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "edited");
    std::fs::write(path.join("src.txt"), "my work").unwrap();

    let result = prune_real(&main, false);

    assert_eq!(state_of(&result, &path).state, State::LocalChanges { changed: 1 });
    assert_eq!(std::fs::read_to_string(path.join("src.txt")).unwrap(), "my work");
}

#[test]
fn should_keep_a_merged_worktree_with_an_untracked_file() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "scratch");
    std::fs::write(path.join("notes.txt"), "todo").unwrap();

    let result = prune_real(&main, false);

    assert!(matches!(state_of(&result, &path).state, State::LocalChanges { .. }));
    assert!(path.exists());
}

#[test]
fn should_keep_a_merged_worktree_with_an_ignored_file() {
    let (_fixture, main) = repository();
    std::fs::write(main.join(".gitignore"), "*.env\n").unwrap();
    run(&main, &["add", ".gitignore"]);
    run(&main, &["commit", "-q", "-m", "ignore"]);
    let path = add_worktree(&main, "env");
    std::fs::write(path.join("local.env"), "SECRET=1").unwrap();

    let result = prune_real(&main, false);

    assert!(matches!(state_of(&result, &path).state, State::LocalChanges { .. }));
    assert!(path.join("local.env").exists(), "an ignored file is not discarded");
}

#[test]
fn should_keep_a_worktree_outside_every_scan_root() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "elsewhere");

    // Only the main checkout is inside the root; the sibling worktree resolves outside it.
    let result = prune(
        std::slice::from_ref(&main),
        std::slice::from_ref(&main),
        WorktreeOptions {
            dry_run: false,
            stale_after: None,
            github_merged: false,
            aggressive: false,
        },
    );

    assert_eq!(state_of(&result, &path).state, State::OutsideRoot);
    assert!(path.exists());
}

#[test]
fn should_keep_a_worktree_whose_commits_are_not_in_the_default_branch() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "ahead");
    std::fs::write(path.join("new.txt"), "x").unwrap();
    run(&path, &["add", "new.txt"]);
    run(&path, &["commit", "-q", "-m", "ahead"]);

    let result = prune_real(&main, false);

    assert_eq!(state_of(&result, &path).state, State::NotMerged);
    assert!(path.exists());
}

#[test]
fn should_keep_a_locked_worktree() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "locked");
    run(&main, &["worktree", "lock", path.to_str().unwrap()]);

    let result = prune_real(&main, false);

    assert_eq!(state_of(&result, &path).state, State::Locked);
    assert!(path.exists());
}

#[test]
fn should_handle_a_repository_reached_through_several_of_its_worktrees_once() {
    let (_fixture, main) = repository();
    let first = add_worktree(&main, "one");
    let second = add_worktree(&main, "two");

    let result = prune(
        &[main.clone(), first.clone(), second.clone()],
        &[main.parent().expect("a parent").to_path_buf()],
        WorktreeOptions {
            dry_run: true,
            stale_after: None,
            github_merged: false,
            aggressive: false,
        },
    );

    assert_eq!(result.repositories.len(), 1);
    assert_eq!(result.repositories[0].worktrees.len(), 2);
}

#[test]
fn should_classify_status_entries() {
    let status = b" D dist/a.js\0 D src/b.rs\0?? new.txt\0R  new.rs\0old.rs\0";
    assert_eq!(classify_status(status), (1, 3));
}

/// An unmerged branch worktree whose only commit is `days` old, with its reflog aged to match.
fn unmerged_worktree(main: &Path, name: &str, days: u64) -> PathBuf {
    let path = main.parent().unwrap().join(name);
    let target = crate::git::for_git(&path).display().to_string();
    run(main, &["worktree", "add", "-q", "-b", name, target.as_str()]);
    std::fs::write(path.join("work.txt"), "wip").unwrap();
    run(&path, &["add", "work.txt"]);
    let then = std::time::SystemTime::now() - Duration::from_secs(days * 86_400);
    let stamp = then.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let date = format!("{stamp} +0000");
    let status = Command::new("git")
        .current_dir(&path)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "wip",
        ])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_AUTHOR_DATE", &date)
        .status()
        .unwrap();
    assert!(status.success());
    let git_dir = git(&path, &["rev-parse", "--path-format=absolute", "--git-dir"]).unwrap();
    crate::testing::set_modified(&Path::new(git_dir.trim()).join("logs/HEAD"), then);
    path
}

fn prune_stale(main: &Path, days: u64) -> WorktreePruneResult {
    let root = main.parent().unwrap().to_path_buf();
    prune(
        &[main.to_path_buf()],
        &[root],
        WorktreeOptions {
            dry_run: false,
            stale_after: Some(Duration::from_secs(days * 86_400)),
            github_merged: false,
            aggressive: false,
        },
    )
}

#[test]
fn should_keep_an_unmerged_worktree_when_no_stale_age_is_given() {
    let (_fixture, main) = repository();
    let path = unmerged_worktree(&main, "old-work", 90);

    let result = prune_real(&main, false);

    assert_eq!(state_of(&result, &path).state, State::NotMerged);
    assert!(path.exists());
}

#[test]
fn should_remove_an_unmerged_worktree_idle_past_the_stale_age_and_keep_its_branch() {
    let (_fixture, main) = repository();
    let path = unmerged_worktree(&main, "old-work", 90);

    let result = prune_stale(&main, 30);

    assert_eq!(state_of(&result, &path).state, State::Stale { discarded: 0 });
    assert!(!path.exists());
    assert!(git(&main, &["rev-parse", "--verify", "refs/heads/old-work"]).is_some());
}

#[test]
fn should_keep_an_unmerged_worktree_that_is_not_yet_stale() {
    let (_fixture, main) = repository();
    let path = unmerged_worktree(&main, "recent-work", 5);

    let result = prune_stale(&main, 30);

    assert_eq!(state_of(&result, &path).state, State::NotMerged);
    assert!(path.exists());
}

#[test]
fn should_keep_a_stale_worktree_that_has_uncommitted_work() {
    let (_fixture, main) = repository();
    let path = unmerged_worktree(&main, "old-work", 90);
    std::fs::write(path.join("work.txt"), "more").unwrap();

    let result = prune_stale(&main, 30);

    assert_eq!(state_of(&result, &path).state, State::LocalChanges { changed: 1 });
    assert!(path.exists());
}

/// Removal keeps branches, not detached commits: a `HEAD` no ref reaches would be lost with it.
#[test]
fn should_never_treat_a_detached_head_no_ref_reaches_as_stale() {
    let (_fixture, main) = repository();
    let path = unmerged_worktree(&main, "old-work", 90);
    run(&path, &["checkout", "-q", "--detach"]);
    run(&main, &["branch", "-q", "-D", "-f", "old-work"]);
    let git_dir = git(&path, &["rev-parse", "--path-format=absolute", "--git-dir"]).unwrap();
    crate::testing::set_modified(
        &Path::new(git_dir.trim()).join("logs/HEAD"),
        std::time::SystemTime::now() - Duration::from_secs(90 * 86_400),
    );

    let result = prune_stale(&main, 30);

    assert_eq!(state_of(&result, &path).state, State::NotMerged);
    assert!(path.exists());
}

#[test]
fn should_require_exact_merged_pull_head_and_base_repository() {
    let head = "0123456789012345678901234567890123456789";
    let response = serde_json::json!([{
        "head": {"sha": head}, "merged_at": "2026-10-14T00:00:00Z",
        "base": {"repo": {"full_name": "Goldziher/voom"}}
    }]);
    let hosted = |response: &serde_json::Value, repository: &str, head: &str| {
        github::classify(response.to_string().as_bytes(), repository, head).unwrap()
    };
    assert_eq!(hosted(&response, "Goldziher/voom", head), github::Hosted::Merged);
    assert_eq!(hosted(&response, "someone/voom", head), github::Hosted::NotMerged);
    assert_ne!(hosted(&response, "Goldziher/voom", "other"), github::Hosted::Merged);
    let mut closed = response;
    closed[0]["merged_at"] = serde_json::Value::Null;
    assert_eq!(hosted(&closed, "Goldziher/voom", head), github::Hosted::NotMerged);
}

fn detached_ahead(main: &Path, name: &str) -> PathBuf {
    let path = add_worktree(main, name);
    std::fs::write(path.join("new.txt"), "work").unwrap();
    run(&path, &["add", "new.txt"]);
    run(&path, &["commit", "-q", "-m", "squashed work"]);
    path
}

fn judge_hosted(main: &Path, path: &Path, dry_run: bool, proof: &Result<bool, String>) -> Worktree {
    let proof = proof.clone().map(|merged| {
        if merged {
            github::Hosted::Merged
        } else {
            github::Hosted::NotMerged
        }
    });
    judge_with(main, path, dry_run, &proof)
}

fn judge_with(main: &Path, path: &Path, dry_run: bool, proof: &Result<github::Hosted, String>) -> Worktree {
    let entries = parse_listing(&git(main, &["worktree", "list", "--porcelain"]).unwrap());
    let entry = entries
        .iter()
        .find(|entry| entry.path.canonicalize().is_ok_and(|canonical| canonical == path))
        .expect("the canonical worktree path is listed");
    let (_, sha) = default_branch(main).unwrap();
    judge(
        entry,
        main,
        MergeProof {
            default_sha: Some(&sha),
            hosted_merge: &|_, _| proof.clone(),
        },
        &[main.parent().unwrap().to_path_buf()],
        None,
        WorktreeOptions {
            dry_run,
            stale_after: None,
            github_merged: true,
            aggressive: false,
        },
        &std::sync::Mutex::new(()),
    )
}

#[test]
fn should_remove_a_hosted_merged_detached_head_and_retain_its_commits() {
    let (_fixture, main) = repository();
    let path = detached_ahead(&main, "squashed");
    let head = git(&path, &["rev-parse", "HEAD"]).unwrap();
    let reference = format!("refs/voom/merged-worktrees/{}", head.trim());
    let worktree = judge_hosted(&main, &path, false, &Ok(true));
    assert_eq!(worktree.outcome, Some(Outcome::Removed));
    assert!(!path.exists());
    assert_eq!(git(&main, &["rev-parse", &reference]), Some(head));
}

#[test]
fn should_not_write_retention_refs_during_a_hosted_merge_dry_run() {
    let (_fixture, main) = repository();
    let path = detached_ahead(&main, "squashed");
    let worktree = judge_hosted(&main, &path, true, &Ok(true));
    assert_eq!(worktree.outcome, Some(Outcome::WouldRemove));
    assert!(path.exists());
    assert_eq!(git(&main, &["for-each-ref", "refs/voom/"]).unwrap(), "");
}

#[test]
fn should_keep_a_hosted_merged_worktree_with_local_or_ignored_work() {
    for (name, contents) in [("notes.txt", "notes"), ("src.txt", "modified"), ("local.env", "secret")] {
        let (_fixture, main) = repository();
        std::fs::write(main.join(".gitignore"), "*.env\n").unwrap();
        run(&main, &["add", ".gitignore"]);
        run(&main, &["commit", "-q", "-m", "ignore"]);
        let path = detached_ahead(&main, "squashed");
        std::fs::write(path.join(name), contents).unwrap();
        let worktree = judge_hosted(&main, &path, false, &Ok(true));
        assert!(matches!(worktree.state, State::LocalChanges { .. }));
        assert!(path.join(name).exists());
        assert_eq!(git(&main, &["for-each-ref", "refs/voom/"]).unwrap(), "");
    }
}

#[test]
fn should_keep_a_worktree_when_hosted_merge_proof_fails() {
    let (_fixture, main) = repository();
    let path = detached_ahead(&main, "squashed");
    let worktree = judge_hosted(&main, &path, false, &Err("network error".to_owned()));
    assert_eq!(worktree.state, State::Unchecked("network error".to_owned()));
    assert!(path.exists());
}

#[test]
fn should_discard_marker_proven_ignored_dependencies_and_keep_unproven_trees() {
    for (artifact, marker) in [
        ("packages/elixir/deps", "mix.exs"),
        ("packages/python/.venv", "pyproject.toml"),
    ] {
        for proven in [false, true] {
            let (_fixture, main) = repository();
            std::fs::write(main.join(".gitignore"), format!("{artifact}/\n")).unwrap();
            let parent = main.join(artifact).parent().unwrap().to_path_buf();
            std::fs::create_dir_all(&parent).unwrap();
            if proven {
                std::fs::write(parent.join(marker), "manifest").unwrap();
            }
            run(&main, &["add", "."]);
            run(&main, &["commit", "-q", "-m", "ignore dependency"]);
            let path = detached_ahead(&main, "squashed");
            std::fs::create_dir_all(path.join(artifact)).unwrap();
            std::fs::write(path.join(artifact).join("contents"), "dependency").unwrap();
            let worktree = judge_hosted(&main, &path, false, &Ok(true));
            if proven {
                assert_eq!(worktree.outcome, Some(Outcome::Removed));
                assert!(!path.exists());
            } else {
                assert!(matches!(worktree.state, State::LocalChanges { .. }));
                assert!(path.join(artifact).exists());
            }
        }
    }
}

#[test]
fn should_refuse_a_head_changed_while_hosted_merge_proof_was_checked() {
    let (_fixture, main) = repository();
    let path = detached_ahead(&main, "squashed");
    let entries = parse_listing(&git(&main, &["worktree", "list", "--porcelain"]).unwrap());
    let entry = entries
        .iter()
        .find(|entry| entry.path.canonicalize().is_ok_and(|canonical| canonical == path))
        .expect("the canonical worktree path is listed");
    let worktree = judge(
        entry,
        &main,
        MergeProof {
            default_sha: None,
            hosted_merge: &|_, _| {
                std::fs::write(path.join("new.txt"), "new work").unwrap();
                run(&path, &["add", "new.txt"]);
                run(&path, &["commit", "-q", "-m", "new work"]);
                Ok(github::Hosted::Merged)
            },
        },
        &[main.parent().unwrap().to_path_buf()],
        None,
        WorktreeOptions {
            github_merged: true,
            aggressive: false,
            ..WorktreeOptions::default()
        },
        &std::sync::Mutex::new(()),
    );
    assert!(matches!(worktree.state, State::Unchecked(_)));
    assert!(path.exists());
    assert_eq!(git(&main, &["for-each-ref", "refs/voom/"]).unwrap(), "");
}

#[test]
fn should_keep_ignored_files_that_share_a_dependency_directory_name() {
    for (name, marker) in [(".venv", "pyproject.toml"), ("deps", "mix.exs")] {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join(marker), "manifest").unwrap();
        std::fs::write(fixture.path().join(name), "user data").unwrap();
        assert_eq!(
            classify_status_in(format!("!! {name}\0").as_bytes(), Some(fixture.path())),
            (0, 1)
        );
    }
}

/// The verdict a single full `git status` gives, which `working_tree` must agree with.
fn full_status_verdict(path: &Path) -> (usize, usize) {
    let status = git_bytes(
        path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignored=matching",
        ],
    )
    .expect("status runs");
    classify_status_in(&status, Some(path))
}

#[test]
fn should_reach_the_verdict_of_a_full_status_in_every_working_tree_state() {
    type Mutation = fn(&Path);
    let cases: &[(&str, Mutation)] = &[
        ("clean", |_| {}),
        ("deleted dist", |path| {
            std::fs::remove_file(path.join("dist/app.js")).unwrap();
        }),
        ("deleted source", |path| {
            std::fs::remove_file(path.join("src.txt")).unwrap();
        }),
        ("modified", |path| {
            std::fs::write(path.join("src.txt"), "edited").unwrap();
        }),
        ("staged", |path| {
            std::fs::write(path.join("src.txt"), "edited").unwrap();
            run(path, &["add", "src.txt"]);
        }),
        ("untracked", |path| std::fs::write(path.join("notes.txt"), "n").unwrap()),
        ("untracked directory", |path| {
            std::fs::create_dir_all(path.join("scratch/deep")).unwrap();
            std::fs::write(path.join("scratch/deep/a"), "n").unwrap();
        }),
        ("empty untracked directory", |path| {
            std::fs::create_dir_all(path.join("empty")).unwrap();
        }),
        ("ignored env", |path| {
            std::fs::write(path.join(".gitignore"), "*.env\n").unwrap();
            run(path, &["add", ".gitignore"]);
            run(path, &["commit", "-q", "-m", "ignore"]);
            std::fs::write(path.join("local.env"), "secret").unwrap();
        }),
        ("ignored build output", |path| {
            std::fs::write(path.join(".gitignore"), "target/\n").unwrap();
            run(path, &["add", ".gitignore"]);
            run(path, &["commit", "-q", "-m", "ignore"]);
            std::fs::create_dir_all(path.join("target/debug")).unwrap();
            std::fs::write(path.join("target/debug/out"), "bin").unwrap();
        }),
        ("ignored build output and an ignored env", |path| {
            std::fs::write(path.join(".gitignore"), "target/\n*.env\n").unwrap();
            run(path, &["add", ".gitignore"]);
            run(path, &["commit", "-q", "-m", "ignore"]);
            std::fs::create_dir_all(path.join("target")).unwrap();
            std::fs::write(path.join("target/out"), "bin").unwrap();
            std::fs::write(path.join("a.env"), "secret").unwrap();
        }),
        ("a nested repository", |path| {
            std::fs::create_dir_all(path.join("vendor-clone")).unwrap();
            run(&path.join("vendor-clone"), &["init", "-q"]);
        }),
    ];
    for (name, mutate) in cases {
        let (_fixture, main) = repository();
        let path = add_worktree(&main, "probe");
        mutate(&path);

        let staged = working_tree(&path).expect("status runs");
        let full = full_status_verdict(&path);

        assert_eq!(staged.1 > 0, full.1 > 0, "{name}: staged {staged:?}, full {full:?}");
        if full.1 == 0 {
            assert_eq!(staged, full, "{name}: a clean checkout counts the same discards");
        }
    }
}

#[test]
fn should_end_at_the_tracked_stage_when_a_tracked_file_changed() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "edited");
    std::fs::write(path.join("src.txt"), "edited").unwrap();
    // A named, ignored, untracked file would also count; the first stage must not need it.
    std::fs::write(path.join("also.txt"), "untracked").unwrap();

    assert_eq!(working_tree(&path), Some((0, 1)), "the untracked file is never read");
}

#[test]
fn should_keep_a_merged_worktree_when_the_pull_request_head_differs_from_its_head() {
    let (_fixture, main) = repository();
    let path = detached_ahead(&main, "amended");

    let worktree = judge_with(&main, &path, false, &Ok(github::Hosted::HeadDiffers { pull: 42 }));

    assert_eq!(worktree.state, State::HeadDiffers { pull: 42 });
    assert_eq!(worktree.outcome, None);
    assert!(path.exists());
}

/// Strips the minimal ANSI the report adds, so assertions read as the user does.
fn plain(bytes: Vec<u8>) -> String {
    anstream::adapter::strip_str(&String::from_utf8(bytes).unwrap()).to_string()
}

fn rendered(result: &WorktreePruneResult, verbose: bool) -> String {
    let mut out = Vec::new();
    render_human(result, verbose, &mut out).unwrap();
    plain(out)
}

#[test]
fn should_name_every_kept_worktree_with_its_reason_under_verbose_only() {
    let (_fixture, main) = repository();
    let merged_dirty = add_worktree(&main, "dirty");
    std::fs::write(merged_dirty.join("notes.txt"), "n").unwrap();
    let locked = add_worktree(&main, "locked");
    run(&main, &["worktree", "lock", locked.to_str().unwrap()]);
    let unmerged = detached_ahead(&main, "ahead");
    let result = prune_real(&main, true);

    let quiet = rendered(&result, false);
    let verbose = rendered(&result, true);

    assert!(!quiet.contains("locked"), "{quiet}");
    assert!(!quiet.contains(&unmerged.display().to_string()), "{quiet}");
    assert!(quiet.contains("changed path(s)"), "{quiet}");
    let line = |name: &str| {
        verbose
            .lines()
            .find(|line| line.contains(name))
            .unwrap_or_else(|| panic!("{name} is listed: {verbose}"))
            .to_owned()
    };
    assert!(line(&locked.display().to_string()).contains("locked"), "{verbose}");
    assert!(
        line(&unmerged.display().to_string()).contains("HEAD is not in main"),
        "{verbose}"
    );
    assert!(
        line(&merged_dirty.display().to_string()).contains("changed path(s)"),
        "{verbose}"
    );
}

#[test]
fn should_carry_each_worktrees_state_and_reason_in_the_json_document() {
    let (_fixture, main) = repository();
    let locked = add_worktree(&main, "locked");
    run(&main, &["worktree", "lock", locked.to_str().unwrap()]);
    let unmerged = detached_ahead(&main, "ahead");
    let clean = add_worktree(&main, "clean");

    let document = document(&prune_real(&main, true));

    let worktrees = document["repositories"][0]["worktrees"].as_array().unwrap();
    let find = |path: &Path| {
        worktrees
            .iter()
            .find(|worktree| worktree["path"] == path.display().to_string())
            .expect("listed")
    };
    assert_eq!(find(&locked)["state"], "locked");
    assert!(find(&locked)["reason"].as_str().unwrap().contains("locked"));
    assert_eq!(find(&unmerged)["state"], "not_merged");
    assert_eq!(find(&unmerged)["reason"], "HEAD is not in main");
    assert_eq!(find(&clean)["state"], "merged");
    assert!(find(&clean)["reason"].is_null());
}

/// A checkout whose `<main>/.git/worktrees/<name>` was deleted.
fn orphaned_checkout(main: &Path, name: &str) -> PathBuf {
    let path = add_worktree(main, name);
    std::fs::write(path.join("unsaved.txt"), "only copy").unwrap();
    std::fs::remove_dir_all(main.join(".git/worktrees").join(name)).unwrap();
    path
}

fn prune_discovered(main: &Path, discovered: &[PathBuf], dry_run: bool) -> WorktreePruneResult {
    prune(
        discovered,
        &[main.parent().unwrap().to_path_buf()],
        WorktreeOptions {
            dry_run,
            ..WorktreeOptions::default()
        },
    )
}

#[test]
fn should_report_a_checkout_whose_git_administration_is_gone_and_never_remove_it() {
    let (_fixture, main) = repository();
    let path = orphaned_checkout(&main, "stray");

    for dry_run in [true, false] {
        let result = prune_discovered(&main, &[main.clone(), path.clone()], dry_run);

        let worktree = state_of(&result, &path);
        assert!(
            matches!(&worktree.state, State::Dangling { bytes, .. } if *bytes > 0),
            "{worktree:?}"
        );
        assert_eq!(worktree.outcome, None);
        assert!(
            path.join("unsaved.txt").exists(),
            "the only copy of the work is untouched"
        );
        assert_eq!(result.totals().dangling, 1);
        assert_eq!(result.totals().removed, 0);
    }
}

#[test]
fn should_explain_a_dangling_checkout_even_without_verbose() {
    let (_fixture, main) = repository();
    let path = orphaned_checkout(&main, "stray");

    let text = rendered(&prune_discovered(&main, &[main.clone(), path], true), false);

    assert!(text.contains("dangling"), "{text}");
    assert!(text.contains("not removed"), "{text}");
    assert!(text.contains("delete it by hand"), "{text}");
}

#[test]
fn should_keep_a_dangling_checkout_outside_every_root_as_outside_the_root() {
    let (_fixture, main) = repository();
    let path = orphaned_checkout(&main, "stray");
    let elsewhere = tempfile::tempdir().unwrap();

    let result = prune(
        &[main.clone(), path.clone()],
        &[elsewhere.path().canonicalize().unwrap()],
        WorktreeOptions::default(),
    );

    assert_eq!(state_of(&result, &path).state, State::OutsideRoot);
}

#[test]
fn should_ignore_a_directory_git_cannot_open_that_is_not_an_orphaned_checkout() {
    let (_fixture, main) = repository();
    let plain_directory = main.parent().unwrap().join("not-a-repo");
    std::fs::create_dir_all(&plain_directory).unwrap();

    let result = prune_discovered(&main, &[main.clone(), plain_directory], true);

    assert!(
        result
            .repositories
            .iter()
            .all(|repository| repository.worktrees.is_empty())
    );
}
