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
    assert!(github::merged_response(response.to_string().as_bytes(), "Goldziher/voom", head).unwrap());
    assert!(!github::merged_response(response.to_string().as_bytes(), "someone/voom", head).unwrap());
    assert!(!github::merged_response(response.to_string().as_bytes(), "Goldziher/voom", "other").unwrap());
    let mut closed = response;
    closed[0]["merged_at"] = serde_json::Value::Null;
    assert!(!github::merged_response(closed.to_string().as_bytes(), "Goldziher/voom", head).unwrap());
}

fn detached_ahead(main: &Path, name: &str) -> PathBuf {
    let path = add_worktree(main, name);
    std::fs::write(path.join("new.txt"), "work").unwrap();
    run(&path, &["add", "new.txt"]);
    run(&path, &["commit", "-q", "-m", "squashed work"]);
    path
}

fn judge_hosted(main: &Path, path: &Path, dry_run: bool, proof: &Result<bool, String>) -> Worktree {
    let entries = parse_listing(&git(main, &["worktree", "list", "--porcelain"]).unwrap());
    let entry = entries.iter().find(|entry| entry.path == path).unwrap();
    let (_, sha) = default_branch(main).unwrap();
    judge(
        entry,
        main,
        MergeProof {
            default_sha: Some(&sha),
            hosted_merge: &|_| proof.clone(),
        },
        &[main.parent().unwrap().to_path_buf()],
        None,
        WorktreeOptions {
            dry_run,
            stale_after: None,
            github_merged: true,
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
    let entry = entries.iter().find(|entry| entry.path == path).unwrap();
    let worktree = judge(
        entry,
        &main,
        MergeProof {
            default_sha: None,
            hosted_merge: &|_| {
                std::fs::write(path.join("new.txt"), "new work").unwrap();
                run(&path, &["add", "new.txt"]);
                run(&path, &["commit", "-q", "-m", "new work"]);
                Ok(true)
            },
        },
        &[main.parent().unwrap().to_path_buf()],
        None,
        WorktreeOptions {
            github_merged: true,
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
