use super::*;

fn aggressive(main: &Path, dry_run: bool) -> WorktreePruneResult {
    prune(
        &[main.to_path_buf()],
        &[main.parent().unwrap().to_path_buf()],
        WorktreeOptions {
            dry_run,
            aggressive: true,
            ..WorktreeOptions::default()
        },
    )
}

#[test]
fn should_discard_merged_local_changes_only_with_aggressive() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "dirty");
    std::fs::write(path.join("src.txt"), "finished work").unwrap();
    std::fs::write(path.join("notes.txt"), "finished notes").unwrap();
    assert_eq!(state_of(&prune_real(&main, false), &path).outcome, None);

    let dry = aggressive(&main, true);
    assert_eq!(state_of(&dry, &path).outcome, Some(Outcome::WouldRemove));
    assert_eq!(std::fs::read_to_string(path.join("src.txt")).unwrap(), "finished work");

    let result = aggressive(&main, false);
    assert_eq!(state_of(&result, &path).outcome, Some(Outcome::Removed));
    assert_eq!(result.totals().removed, 1);
    assert!(!path.exists());
    assert!(main.join("src.txt").exists(), "main checkout survives");
}

#[test]
fn should_keep_unmerged_work_even_with_aggressive() {
    let (_fixture, main) = repository();
    let path = detached_ahead(&main, "unmerged");
    std::fs::write(path.join("src.txt"), "finished work").unwrap();
    let result = aggressive(&main, false);
    assert_eq!(state_of(&result, &path).state, State::NotMerged);
    assert!(path.exists());
}

#[test]
fn should_keep_locked_worktrees_even_with_aggressive() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "locked");
    run(&main, &["worktree", "lock", path.to_str().unwrap()]);
    let result = aggressive(&main, false);
    assert_eq!(state_of(&result, &path).state, State::Locked);
    assert!(path.exists());
}

#[test]
fn should_remove_a_dangling_checkout_only_with_aggressive() {
    let (_fixture, main) = repository();
    let path = orphaned_checkout(&main, "stray");
    let discovered = [main.clone(), path.clone()];
    for dry_run in [true, false] {
        let result = prune(
            &discovered,
            &[main.parent().unwrap().to_path_buf()],
            WorktreeOptions {
                dry_run,
                aggressive: true,
                ..WorktreeOptions::default()
            },
        );
        assert_eq!(
            state_of(&result, &path).outcome,
            Some(if dry_run {
                Outcome::WouldRemove
            } else {
                Outcome::Removed
            })
        );
        assert_eq!(result.totals().removed, 1);
        assert_eq!(result.totals().dangling, 0);
        assert_eq!(path.exists(), dry_run);
    }
}

#[test]
fn should_keep_a_dangling_checkout_outside_the_roots_even_with_aggressive() {
    let (_fixture, main) = repository();
    let path = orphaned_checkout(&main, "stray");
    let result = prune(
        std::slice::from_ref(&path),
        std::slice::from_ref(&main),
        WorktreeOptions {
            aggressive: true,
            ..WorktreeOptions::default()
        },
    );
    assert_eq!(state_of(&result, &path).state, State::OutsideRoot);
    assert!(path.join("unsaved.txt").exists());
}

#[test]
fn should_keep_the_current_checkout_even_with_aggressive() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "current");
    let listing = git(&main, &["worktree", "list", "--porcelain"]).unwrap();
    let entries = parse_listing(&listing);
    let entry = entries
        .iter()
        .find(|entry| entry.path.canonicalize().is_ok_and(|canonical| canonical == path))
        .expect("the canonical worktree path is listed");
    let (_, sha) = default_branch(&main).unwrap();
    let hosted = |_: &str, _: Option<&str>| Ok(github::Hosted::NotMerged);
    let result = judge(
        entry,
        &JudgeContext {
            main: &main,
            merge_proof: MergeProof {
                default_sha: Some(&sha),
                hosted_merge: &hosted,
            },
            roots: &[main.parent().unwrap().to_path_buf()],
            current: Some(&path),
            options: WorktreeOptions {
                aggressive: true,
                ..WorktreeOptions::default()
            },
            removal_lock: &std::sync::Mutex::new(()),
        },
    );
    assert_eq!(result.state, State::Current);
    assert!(path.exists());
}

#[test]
fn should_discard_reachable_unmerged_work_with_an_explicit_zero_stale_age() {
    let (_fixture, main) = repository();
    let path = unmerged_worktree(&main, "finished", 0);
    std::fs::write(path.join("work.txt"), "finished local work").unwrap();
    let head = git(&path, &["rev-parse", "HEAD"]).unwrap();
    let result = prune(
        std::slice::from_ref(&main),
        &[main.parent().unwrap().to_path_buf()],
        WorktreeOptions {
            aggressive: true,
            stale_after: Some(Duration::ZERO),
            ..WorktreeOptions::default()
        },
    );
    assert_eq!(state_of(&result, &path).outcome, Some(Outcome::Removed));
    assert!(!path.exists());
    assert_eq!(git(&main, &["rev-parse", "refs/heads/finished"]).unwrap(), head);
}

#[test]
fn should_retain_detached_commits_before_aggressive_removal_without_writing_refs_in_dry_run() {
    let (_fixture, main) = repository();
    let path = detached_ahead(&main, "unreachable");
    let head = git(&path, &["rev-parse", "HEAD"]).unwrap();
    let reference = format!("refs/voom/aggressive-worktrees/{}", head.trim());
    for dry_run in [true, false] {
        let result = prune(
            std::slice::from_ref(&main),
            &[main.parent().unwrap().to_path_buf()],
            WorktreeOptions {
                aggressive: true,
                dry_run,
                stale_after: Some(Duration::ZERO),
                ..WorktreeOptions::default()
            },
        );
        assert_eq!(
            state_of(&result, &path).outcome,
            Some(if dry_run {
                Outcome::WouldRemove
            } else {
                Outcome::Removed
            })
        );
        assert_eq!(path.exists(), dry_run);
        assert_eq!(
            git(&main, &["rev-parse", "--verify", &reference]),
            if dry_run { None } else { Some(head.clone()) }
        );
    }
}

#[test]
fn should_keep_an_aggressive_checkout_when_retaining_its_detached_head_fails() {
    let (_fixture, main) = repository();
    let path = detached_ahead(&main, "unreachable");
    let base = git(&main, &["rev-parse", "HEAD"]).unwrap();
    std::fs::create_dir_all(main.join(".git/refs/voom")).unwrap();
    std::fs::write(main.join(".git/refs/voom/aggressive-worktrees"), base).unwrap();
    let result = prune(
        std::slice::from_ref(&main),
        &[main.parent().unwrap().to_path_buf()],
        WorktreeOptions {
            aggressive: true,
            stale_after: Some(Duration::ZERO),
            ..WorktreeOptions::default()
        },
    );
    assert_eq!(
        state_of(&result, &path).outcome,
        Some(Outcome::Failed("could not retain worktree HEAD".to_owned()))
    );
    assert!(path.join("new.txt").exists());
}

#[test]
fn should_respect_an_explicit_stale_age_for_an_aggressive_detached_checkout() {
    let (_fixture, main) = repository();
    let path = detached_ahead(&main, "recent");
    let result = prune(
        std::slice::from_ref(&main),
        &[main.parent().unwrap().to_path_buf()],
        WorktreeOptions {
            aggressive: true,
            stale_after: Some(Duration::from_secs(86400)),
            ..WorktreeOptions::default()
        },
    );
    assert_eq!(state_of(&result, &path).state, State::NotMerged);
    assert!(path.exists());
}

#[test]
fn should_discard_ignored_evidence_only_with_aggressive() {
    let (_fixture, main) = repository();
    std::fs::write(main.join(".gitignore"), "*.env\n").unwrap();
    run(&main, &["add", ".gitignore"]);
    run(&main, &["commit", "-q", "-m", "ignore"]);
    let path = add_worktree(&main, "finished");
    std::fs::write(path.join("local.env"), "private evidence").unwrap();
    assert_eq!(
        state_of(&prune_real(&main, false), &path).state,
        State::LocalChanges { changed: 1 }
    );
    let result = aggressive(&main, false);
    assert_eq!(state_of(&result, &path).outcome, Some(Outcome::Removed));
    assert!(!path.exists());
}

#[test]
fn should_keep_the_current_dangling_checkout_even_with_aggressive() {
    let (_fixture, main) = repository();
    let path = orphaned_checkout(&main, "stray");
    let orphan = dangling::orphan(&path).unwrap();
    let mut found = Vec::new();
    dangling::attach(
        &mut found,
        &path,
        &orphan,
        &[main.parent().unwrap().to_path_buf()],
        Some(&path),
        WorktreeOptions {
            aggressive: true,
            ..WorktreeOptions::default()
        },
    );
    assert_eq!(found[0].worktrees[0].state, State::Current);
    assert!(path.join("unsaved.txt").exists());
}

#[test]
fn should_keep_a_registered_checkout_outside_the_roots_even_with_aggressive() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "outside");
    std::fs::write(path.join("src.txt"), "finished work").unwrap();
    let result = prune(
        std::slice::from_ref(&main),
        std::slice::from_ref(&main),
        WorktreeOptions {
            aggressive: true,
            ..WorktreeOptions::default()
        },
    );
    assert_eq!(state_of(&result, &path).state, State::OutsideRoot);
    assert!(path.exists());
}

#[test]
#[cfg(unix)]
fn should_refuse_a_symlink_to_a_dangling_checkout_even_with_aggressive() {
    let (_fixture, main) = repository();
    let path = orphaned_checkout(&main, "stray");
    let alias = main.parent().unwrap().join("alias");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let result = prune(
        &[alias],
        &[main.parent().unwrap().to_path_buf()],
        WorktreeOptions {
            aggressive: true,
            ..WorktreeOptions::default()
        },
    );
    assert!(matches!(state_of(&result, &path).outcome, Some(Outcome::Failed(_))));
    assert!(path.join("unsaved.txt").exists());
}

#[test]
fn should_skip_status_entirely_when_aggressive_discards_local_content() {
    let (_fixture, main) = repository();
    let path = add_worktree(&main, "finished");
    let gitdir = git(&path, &["rev-parse", "--absolute-git-dir"]).unwrap();
    std::fs::write(Path::new(gitdir.trim()).join("index"), "unreadable index").unwrap();
    let safe = prune_real(&main, true);
    assert_eq!(
        state_of(&safe, &path).state,
        State::Unchecked("`git status` failed".to_owned())
    );
    let dry = aggressive(&main, true);
    assert_eq!(state_of(&dry, &path).state, State::Discarded);
    assert_eq!(state_of(&dry, &path).outcome, Some(Outcome::WouldRemove));
    assert!(path.join("src.txt").exists());
}
