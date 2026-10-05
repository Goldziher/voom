use super::*;
use crate::testing::tree;

fn options(root: &Path) -> BazelPruneOptions {
    BazelPruneOptions {
        roots: vec![root.to_path_buf()],
        dry_run: false,
        force: false,
        one_file_system: true,
        max_age: DEFAULT_MAX_AGE,
        clear_all: false,
    }
}

fn age(path: &Path, days: u64) {
    let then = std::time::SystemTime::now() - Duration::from_secs(days * 86_400);
    let file = std::fs::File::options().read(true).open(path).unwrap();
    file.set_modified(then).unwrap();
}

/// Ages every marker file and directory Bazel would have touched, and the base itself.
fn age_base(base: &Path, days: u64) {
    for name in ACTIVITY_MARKERS {
        if base.join(name).exists() {
            age(&base.join(name), days);
        }
    }
    age(base, days);
}

#[test]
fn should_remove_an_output_base_whose_owner_no_longer_exists() {
    let fixture = tree(&[
        "_bazel_dev/abc123/execroot/armis/BUILD",
        "_bazel_dev/abc123/DO_NOT_BUILD_HERE",
    ]);
    let root = fixture.path().join("_bazel_dev");
    std::fs::write(root.join("abc123/DO_NOT_BUILD_HERE"), "/nonexistent/workspace").unwrap();

    let result = prune(&options(&root)).expect("the root resolves");

    assert_eq!(result.output_bases.len(), 1);
    let base = &result.output_bases[0];
    assert!(matches!(&base.state, OutputBaseState::Orphaned { owner } if owner == Path::new("/nonexistent/workspace")));
    assert_eq!(base.outcome, Some(Outcome::Removed));
    assert!(!base.path.exists(), "the orphan is actually gone");
}

#[test]
fn should_leave_an_owned_output_base_alone() {
    let fixture = tree(&["_bazel_dev/abc123/DO_NOT_BUILD_HERE", "workspace/WORKSPACE"]);
    let root = fixture.path().join("_bazel_dev");
    let owner = fixture.path().join("workspace");
    std::fs::write(root.join("abc123/DO_NOT_BUILD_HERE"), owner.display().to_string()).unwrap();

    let result = prune(&options(&root)).expect("the root resolves");

    let base = &result.output_bases[0];
    assert!(matches!(&base.state, OutputBaseState::Owned { .. }));
    assert!(
        base.outcome.is_none(),
        "nothing is even attempted against an owned base"
    );
    assert!(base.path.exists(), "an owned output base survives");
}

#[test]
fn should_leave_an_unmarked_directory_unproven() {
    let fixture = tree(&["_bazel_dev/abc123/execroot/armis/BUILD"]);
    let root = fixture.path().join("_bazel_dev");

    let result = prune(&options(&root)).expect("the root resolves");

    assert_eq!(result.output_bases[0].state, OutputBaseState::Unproven);
}

/// The shared, sequentially-reused root this ADR measured: the marker sits under
/// `execroot/` instead of at the output base's own root, which is read — but a root-level
/// marker still wins when both exist, since it is the more specific claim.
#[test]
fn should_read_the_execroot_marker_only_when_no_root_marker_exists() {
    let fixture = tree(&["_bazel_dev/shared/execroot/DO_NOT_BUILD_HERE"]);
    let root = fixture.path().join("_bazel_dev");
    std::fs::write(
        root.join("shared/execroot/DO_NOT_BUILD_HERE"),
        "/nonexistent/last-builder",
    )
    .unwrap();

    let result = prune(&options(&root)).expect("the root resolves");

    assert!(matches!(
        &result.output_bases[0].state,
        OutputBaseState::Orphaned { owner } if owner == Path::new("/nonexistent/last-builder")
    ));
}

#[test]
fn should_leave_a_missing_root_silently_absent() {
    let fixture = tree(&["keep.txt"]);
    let missing = fixture.path().join("does-not-exist");

    let result = prune(&options(&missing)).expect("a missing root is not an error");

    assert!(result.roots.is_empty());
    assert!(result.output_bases.is_empty());
}

#[test]
fn should_respect_dry_run_and_leave_the_orphan_on_disk() {
    let fixture = tree(&["_bazel_dev/abc123/DO_NOT_BUILD_HERE"]);
    let root = fixture.path().join("_bazel_dev");
    std::fs::write(root.join("abc123/DO_NOT_BUILD_HERE"), "/nonexistent/workspace").unwrap();

    let mut dry = options(&root);
    dry.dry_run = true;
    let result = prune(&dry).expect("the root resolves");

    assert_eq!(result.output_bases[0].outcome, Some(Outcome::WouldRemove));
    assert!(root.join("abc123").exists(), "a dry run removes nothing");
}

#[test]
fn should_remove_a_base_whose_live_workspace_has_not_been_built_in_for_too_long() {
    let fixture = tree(&[
        "_bazel_dev/abc/command.log",
        "_bazel_dev/abc/DO_NOT_BUILD_HERE",
        "workspace/WORKSPACE",
    ]);
    let root = fixture.path().join("_bazel_dev");
    let owner = fixture.path().join("workspace");
    std::fs::write(root.join("abc/DO_NOT_BUILD_HERE"), owner.display().to_string()).unwrap();
    age_base(&root.join("abc"), 30);

    let result = prune(&options(&root)).expect("the root resolves");

    assert!(matches!(
        &result.output_bases[0].state,
        OutputBaseState::Stale { idle, .. } if *idle > Duration::from_secs(29 * 86_400)
    ));
    assert!(!root.join("abc").exists(), "the stale base is gone");
    assert!(owner.exists(), "the workspace itself is never touched");
}

#[test]
fn should_keep_a_base_built_in_recently() {
    let fixture = tree(&[
        "_bazel_dev/abc/command.log",
        "_bazel_dev/abc/DO_NOT_BUILD_HERE",
        "workspace/WORKSPACE",
    ]);
    let root = fixture.path().join("_bazel_dev");
    let owner = fixture.path().join("workspace");
    std::fs::write(root.join("abc/DO_NOT_BUILD_HERE"), owner.display().to_string()).unwrap();

    let result = prune(&options(&root)).expect("the root resolves");

    assert!(matches!(&result.output_bases[0].state, OutputBaseState::Owned { .. }));
    assert!(root.join("abc").exists());
}

#[test]
fn should_remove_a_base_owned_by_a_worktree_git_no_longer_tracks() {
    let fixture = tree(&["_bazel_dev/abc/DO_NOT_BUILD_HERE", "worktree/bazel-bin"]);
    let root = fixture.path().join("_bazel_dev");
    let owner = fixture.path().join("worktree");
    std::fs::write(root.join("abc/DO_NOT_BUILD_HERE"), owner.display().to_string()).unwrap();
    std::fs::write(owner.join(".git"), "gitdir: /nonexistent/repo/.git/worktrees/gone\n").unwrap();

    let result = prune(&options(&root)).expect("the root resolves");

    assert!(matches!(
        &result.output_bases[0].state,
        OutputBaseState::Abandoned { .. }
    ));
    assert!(!root.join("abc").exists());
}

#[test]
fn should_keep_a_base_owned_by_a_worktree_whose_administration_exists() {
    let fixture = tree(&["_bazel_dev/abc/DO_NOT_BUILD_HERE", "worktree/x", "admin/HEAD"]);
    let root = fixture.path().join("_bazel_dev");
    let owner = fixture.path().join("worktree");
    std::fs::write(root.join("abc/DO_NOT_BUILD_HERE"), owner.display().to_string()).unwrap();
    std::fs::write(
        owner.join(".git"),
        format!("gitdir: {}\n", fixture.path().join("admin").display()),
    )
    .unwrap();

    let result = prune(&options(&root)).expect("the root resolves");

    assert!(matches!(&result.output_bases[0].state, OutputBaseState::Owned { .. }));
}

#[test]
fn should_unlink_the_convenience_symlinks_a_removed_base_leaves_in_its_workspace() {
    let fixture = tree(&[
        "_bazel_dev/abc/execroot/x",
        "_bazel_dev/abc/DO_NOT_BUILD_HERE",
        "worktree/.keep",
    ]);
    let root = fixture.path().join("_bazel_dev");
    let owner = fixture.path().join("worktree");
    std::fs::write(root.join("abc/DO_NOT_BUILD_HERE"), owner.display().to_string()).unwrap();
    std::fs::write(owner.join(".git"), "gitdir: /nonexistent\n").unwrap();
    let base = root.join("abc").canonicalize().unwrap();
    std::os::unix::fs::symlink(base.join("execroot"), owner.join("bazel-out")).unwrap();
    std::os::unix::fs::symlink("/somewhere/else", owner.join("bazel-elsewhere")).unwrap();

    prune(&options(&root)).expect("the root resolves");

    assert!(
        owner.join("bazel-out").symlink_metadata().is_err(),
        "the dangling link is gone"
    );
    assert!(
        owner.join("bazel-elsewhere").symlink_metadata().is_ok(),
        "a link pointing elsewhere is not ours"
    );
}

#[test]
fn should_remove_only_install_bases_no_surviving_base_uses() {
    let fixture = tree(&[
        "_bazel_dev/install/used/A-server.jar",
        "_bazel_dev/install/spare/A-server.jar",
        "_bazel_dev/abc/DO_NOT_BUILD_HERE",
        "workspace/WORKSPACE",
    ]);
    let root = fixture.path().join("_bazel_dev");
    let owner = fixture.path().join("workspace");
    std::fs::write(root.join("abc/DO_NOT_BUILD_HERE"), owner.display().to_string()).unwrap();
    std::os::unix::fs::symlink(root.join("install/used"), root.join("abc/install")).unwrap();

    prune(&options(&root)).expect("the root resolves");

    assert!(root.join("install/used").exists());
    assert!(!root.join("install/spare").exists());
}

#[test]
fn should_prune_only_old_files_from_the_shared_cache() {
    let fixture = tree(&[
        "_bazel_dev/cache/repos/v1/old/blob",
        "_bazel_dev/cache/repos/v1/new/blob",
    ]);
    let root = fixture.path().join("_bazel_dev");
    age(&root.join("cache/repos/v1/old/blob"), 30);

    prune(&options(&root)).expect("the root resolves");

    assert!(
        !root.join("cache/repos/v1/old").exists(),
        "the old entry and its directory are gone"
    );
    assert!(root.join("cache/repos/v1/new/blob").exists());
}

#[test]
fn should_clear_everything_under_a_bazel_root_when_asked() {
    let fixture = tree(&[
        "_bazel_dev/abc/DO_NOT_BUILD_HERE",
        "_bazel_dev/unmarked/execroot/x",
        "_bazel_dev/install/used/A-server.jar",
        "_bazel_dev/cache/repos/v1/new/blob",
        "workspace/WORKSPACE",
    ]);
    let root = fixture.path().join("_bazel_dev");
    let owner = fixture.path().join("workspace");
    std::fs::write(root.join("abc/DO_NOT_BUILD_HERE"), owner.display().to_string()).unwrap();
    std::os::unix::fs::symlink(root.join("install/used"), root.join("abc/install")).unwrap();

    let mut clear = options(&root);
    clear.clear_all = true;
    prune(&clear).expect("the root resolves");

    for name in ["abc", "unmarked", "install/used", "cache"] {
        assert!(!root.join(name).exists(), "{name} is cleared");
    }
    assert!(owner.exists());
}

#[test]
fn should_not_clear_an_unmarked_directory_outside_a_bazel_root() {
    let fixture = tree(&["projects/notes/keep.txt"]);
    let root = fixture.path().join("projects");

    let mut clear = options(&root);
    clear.clear_all = true;
    prune(&clear).expect("the root resolves");

    assert!(root.join("notes/keep.txt").exists());
}
