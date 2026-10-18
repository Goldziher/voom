//! The explicit opt-in that enables every artifact and cache category.

use clap::Parser;
use voom::cli::Cli;

#[test]
fn should_enable_every_catalog_artifact_and_cache_aggressively() {
    let cli = Cli::try_parse_from(["voom", "--aggressive", "."]).unwrap();
    let flags = cli.prune.flags().unwrap();
    for ecosystem in voom::catalog::CATALOG {
        assert!(flags.enable.iter().any(|spec| spec == ecosystem.id), "{}", ecosystem.id);
    }
    assert!(flags.clean_caches.iter().any(|id| id == "all"));
    assert!(flags.clean_caches.iter().any(|id| id == "huggingface"));
    assert_eq!(flags.clean_tagged, Some(true));
    assert_eq!(flags.docker, Some(true));
    assert_eq!(flags.docker_max_age, Some(std::time::Duration::ZERO));
    assert!(cli.prune.to_run_options().unwrap().force);
}

#[test]
fn should_accept_worktree_controls_with_aggressive_without_a_second_flag() {
    Cli::try_parse_from(["voom", "--aggressive", "--stale-worktrees", "7d", "."]).unwrap();
    Cli::try_parse_from(["voom", "git-prune", "--aggressive", "--stale-worktrees", "7d", "."]).unwrap();
}

#[test]
fn should_remove_proven_dependencies_and_preserve_unproven_names_aggressively() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("project/node_modules/pkg")).unwrap();
    std::fs::write(root.path().join("project/package.json"), "{}").unwrap();
    std::fs::write(root.path().join("project/node_modules/pkg/index.js"), "dependency").unwrap();
    std::fs::create_dir_all(root.path().join("unproven/node_modules")).unwrap();
    std::fs::write(root.path().join("unproven/node_modules/notes"), "source").unwrap();
    let cli = Cli::try_parse_from(["voom", "--aggressive", "--no-git", "--no-bazel", "."]).unwrap();
    let mut options = cli.prune.to_run_options().unwrap();
    options.roots = vec![root.path().to_path_buf()];
    let result = voom::run::run(&options).unwrap();
    assert_eq!(result.totals().reclaimed, 1);
    assert!(!root.path().join("project/node_modules").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("unproven/node_modules/notes")).unwrap(),
        "source"
    );
}

#[test]
fn should_keep_explicit_disables_and_dry_run_under_aggressive() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("package.json"), "{}").unwrap();
    std::fs::create_dir(root.path().join("node_modules")).unwrap();
    let cli = Cli::try_parse_from([
        "voom",
        "--aggressive",
        "--disable",
        "node.node_modules",
        "--dry-run",
        ".",
    ])
    .unwrap();
    let mut options = cli.prune.to_run_options().unwrap();
    options.roots = vec![root.path().to_path_buf()];
    let result = voom::run::run(&options).unwrap();
    assert_eq!(result.entries.len(), 0);
    assert!(root.path().join("node_modules").exists());
}
