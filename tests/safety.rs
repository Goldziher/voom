//! The properties CONTRIBUTING names as grounds for rejecting a pull request.
//!
//! These tests are the specification. If one starts failing, the behaviour regressed — do not
//! "fix" the test. Each has been checked to actually fail when the property it guards is
//! removed.

// Helper functions here are not `#[test]` bodies, which is all `allow-expect-in-tests` covers.
// In a test crate a panic is the failure report, which is exactly what these want.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::fs;

use support::{options, snapshot};
use tempfile::TempDir;
use voom::delete::Outcome;
use voom::run::{RunOptions, run};

/// Build artifacts are precisely what `.gitignore` lists. A walker that respects ignore files
/// finds nothing and reports a clean machine — a silent total failure nobody would report as a
/// bug. See ADR 0005.
#[test]
fn should_find_a_gitignored_artifact() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join(".git")).unwrap();
    fs::write(root.path().join(".git/HEAD"), b"ref: refs/heads/main").unwrap();
    fs::write(root.path().join(".gitignore"), b"/target\n").unwrap();
    fs::write(root.path().join("Cargo.toml"), b"[package]").unwrap();
    fs::create_dir_all(root.path().join("target/debug")).unwrap();
    fs::write(root.path().join("target/debug/app"), b"output").unwrap();

    let result = run(&options(root.path())).expect("the run completes");

    assert_eq!(result.entries.len(), 1, "a gitignored target/ must still be found");
    assert!(!root.path().join("target").exists());
}

/// A symlinked `target/` pointing at a source tree must not become a deletion of that tree.
#[test]
#[cfg(unix)]
fn should_never_delete_through_a_symlink() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Cargo.toml"), b"[package]").unwrap();
    fs::create_dir_all(root.path().join("source")).unwrap();
    fs::write(root.path().join("source/main.rs"), b"fn main() {}").unwrap();
    std::os::unix::fs::symlink(root.path().join("source"), root.path().join("target")).unwrap();

    run(&options(root.path())).expect("the run completes");

    assert!(
        root.path().join("source/main.rs").exists(),
        "the symlink target survives"
    );
    assert!(root.path().join("target").is_symlink(), "the link itself survives");
}

/// A symlink pointing *outside* the scan root is the dangerous case: following it would delete
/// a tree voom was never pointed at.
#[test]
#[cfg(unix)]
fn should_not_follow_a_symlink_out_of_the_scan_root() {
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("precious.txt"), b"not voom's business").unwrap();

    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Cargo.toml"), b"[package]").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("target")).unwrap();

    run(&options(root.path())).expect("the run completes");

    assert!(
        outside.path().join("precious.txt").exists(),
        "the outside tree is untouched"
    );
}

/// `.git/` is never entered, so nothing inside it can ever be classified.
#[test]
fn should_never_descend_into_a_vcs_directory() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join(".git/target")).unwrap();
    fs::write(root.path().join(".git/Cargo.toml"), b"[package]").unwrap();
    fs::write(root.path().join(".git/target/object"), b"data").unwrap();
    let before = snapshot(root.path());

    let result = run(&options(root.path())).expect("the run completes");

    assert!(result.entries.is_empty());
    assert_eq!(snapshot(root.path()), before);
}

/// Dependency caches are out of scope (ADR 0001) and no flag turns them on.
#[test]
fn should_never_remove_a_dependency_directory() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("package.json"), b"{}").unwrap();
    fs::create_dir_all(root.path().join("node_modules/left-pad")).unwrap();
    fs::write(
        root.path().join("node_modules/left-pad/index.js"),
        b"module.exports = 0",
    )
    .unwrap();

    let mut options = options(root.path());
    // Even asking for everything cannot reach them: they are absent from the catalog.
    options.flags.enable = vec!["node.build".to_owned()];
    run(&options).expect("the run completes");

    assert!(root.path().join("node_modules/left-pad/index.js").exists());
}

/// A `.venv/` next to a `pyproject.toml` is proven, but expensive to rebuild, so it needs an
/// explicit opt-in. This is the `default_on = false` contract end to end.
#[test]
fn should_respect_the_opt_in_for_an_expensive_artifact() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("pyproject.toml"), b"[project]").unwrap();
    fs::create_dir_all(root.path().join(".venv/lib")).unwrap();
    fs::write(root.path().join(".venv/lib/site.py"), b"# packages").unwrap();

    run(&options(root.path())).expect("the run completes");
    assert!(
        root.path().join(".venv/lib/site.py").exists(),
        "a virtualenv is not swept by default"
    );

    let mut options = options(root.path());
    options.flags.enable = vec!["python.venv".to_owned()];
    run(&options).expect("the run completes");
    assert!(!root.path().join(".venv").exists(), "and is swept when asked for");
}

/// The scan root itself is never removed, however well it is proven.
#[test]
fn should_never_remove_the_scan_root() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Cargo.toml"), b"[package]").unwrap();
    let target = root.path().join("target");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("app"), b"output").unwrap();

    // Point voom *at* the artifact. It is now the root, and a root is never its own artifact.
    let mut options = options(&target);
    options.roots = vec![target.clone()];
    run(&options).expect("the run completes");

    assert!(
        target.exists(),
        "the scan root survives even when it looks like an artifact"
    );
}

/// Ambiguous names resolve by ecosystem, not by precedence: `target/` beside a `Cargo.toml` is
/// Rust's, beside a `pom.xml` is Maven's, and beside neither is nobody's.
/// `Ancestor(3)` is the only anchor in the catalog that climbs, and a sweep of a real machine
/// found all 55 .NET artifacts sitting directly beside their project file — so the climb has
/// never actually run outside a classifier unit test. This exercises it end to end, and pins
/// the bound in the same shape, because a wrong bound is either lost recall or a deletion
/// justified by a marker three directories away from what it proved.
#[test]
fn should_sweep_a_nested_dotnet_artifact_and_stop_at_the_ancestor_bound() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("App.csproj"), b"<Project />").unwrap();

    for nesting in ["", "a/", "a/b/", "a/b/c/", "a/b/c/d/"] {
        let obj = root.path().join(format!("{nesting}obj"));
        fs::create_dir_all(&obj).unwrap();
        fs::write(obj.join("build.o"), b"output").unwrap();
    }

    run(&options(root.path())).expect("the run completes");

    for nesting in ["", "a/", "a/b/", "a/b/c/"] {
        assert!(
            !root.path().join(format!("{nesting}obj")).exists(),
            "obj/ {nesting} levels below its .csproj is within the bound and must be swept"
        );
    }
    assert!(
        root.path().join("a/b/c/d/obj/build.o").exists(),
        "four levels down is beyond Ancestor(3) and nothing proves it"
    );
}

#[test]
fn should_resolve_an_ambiguous_name_by_which_marker_is_present() {
    let root = TempDir::new().unwrap();
    for (project, marker) in [("rust", "Cargo.toml"), ("maven", "pom.xml"), ("data", "README.md")] {
        fs::create_dir_all(root.path().join(project).join("target")).unwrap();
        fs::write(root.path().join(project).join(marker), b"marker").unwrap();
        fs::write(root.path().join(project).join("target/contents"), b"x").unwrap();
    }

    let result = run(&options(root.path())).expect("the run completes");

    let mut ecosystems: Vec<_> = result
        .entries
        .iter()
        .filter_map(voom::report::Entry::ecosystem)
        .collect();
    ecosystems.sort_unstable();
    assert_eq!(ecosystems, vec!["maven", "rust"]);
    assert!(
        root.path().join("data/target/contents").exists(),
        "the unproven one survives"
    );
}

/// A run over a tree it cannot fully read reports the failure and keeps going, rather than
/// aborting and leaving a partial result no re-run can resolve.
#[test]
#[cfg(unix)]
fn should_isolate_a_failure_to_the_artifact_it_happened_on() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().unwrap();
    for project in ["ok", "locked"] {
        fs::create_dir_all(root.path().join(project).join("target")).unwrap();
        fs::write(root.path().join(project).join("Cargo.toml"), b"[package]").unwrap();
        fs::write(root.path().join(project).join("target/app"), b"output").unwrap();
    }
    // Make the parent read-only so the removal of `locked/target` cannot succeed.
    let locked = root.path().join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

    let result = run(&options(root.path())).expect("the run completes rather than aborting");

    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        !root.path().join("ok/target").exists(),
        "the healthy artifact is still removed"
    );
    assert_eq!(result.entries.len(), 2, "both are reported");
    // Running as root defeats the permission bits, so only assert once it actually bit.
    let totals = result.totals();
    if totals.failed > 0 || totals.partial > 0 {
        assert_eq!(
            result.exit_code(false),
            1,
            "a removal that did not finish is exit code 1"
        );
    }
}

/// The regression test for the defect this release exists to fix: a sweep that freed 130 GB
/// reported having reclaimed 25, because a removal that fails part-way through contributed
/// nothing at all to the total.
///
/// `remove_dir_all` unlinks a tree's contents before the tree itself, so making the artifact's
/// *parent* read-only empties `target/` completely and then fails to unlink it. That is not a
/// contrived shape — it is what a concurrent build produces, and what this machine produced.
#[test]
#[cfg(unix)]
fn should_report_how_much_a_partial_removal_actually_freed() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    fs::create_dir_all(project.join("target/deep")).unwrap();
    fs::write(project.join("Cargo.toml"), b"[package]").unwrap();
    fs::write(project.join("target/app"), vec![7u8; 8192]).unwrap();
    fs::write(project.join("target/deep/lib"), vec![7u8; 8192]).unwrap();
    fs::set_permissions(&project, fs::Permissions::from_mode(0o555)).unwrap();

    let result = run(&options(root.path())).expect("the run completes");

    fs::set_permissions(&project, fs::Permissions::from_mode(0o755)).unwrap();

    let totals = result.totals();
    if totals.partial == 0 {
        // Running as root defeats the permission bits and the removal simply succeeds.
        return;
    }

    assert_eq!(totals.partial, 1, "the artifact is reported as partly removed");
    assert!(
        totals.bytes > 0,
        "the headline counts what was actually freed, which is the whole point: {totals:?}"
    );
    assert_eq!(
        totals.bytes, totals.partial_bytes,
        "every byte this run freed came from the partial removal"
    );
    assert_eq!(totals.reclaimed, 0, "nothing is gone — the artifact is still there");
    assert!(
        project.join("target").exists(),
        "the artifact directory survives, emptied"
    );
    assert!(
        !project.join("target/app").exists(),
        "its contents did not: that is what `freed` measures"
    );
}

/// `include` bypasses marker proof, not the rails. ADR 0004 calls it a loaded foot-gun by
/// design; ADR 0006's denylist is what stops the gun pointing at a repository's history.
#[test]
fn should_refuse_an_included_path_that_the_denylist_protects() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join(".git")).unwrap();
    fs::write(root.path().join(".git/HEAD"), b"ref: refs/heads/main").unwrap();
    fs::write(
        root.path().join("voom.toml"),
        format!("include = ['{}']\n", root.path().join(".git").display()),
    )
    .unwrap();

    let result = run(&options(root.path())).expect("the run completes");

    assert!(
        root.path().join(".git/HEAD").exists(),
        "no configuration reaches a VCS directory"
    );
    assert!(
        result
            .entries
            .iter()
            .all(|entry| matches!(entry.outcome, Outcome::Refused(_))),
        "and the refusal is reported rather than silent: {:?}",
        result.entries
    );
}

/// Keep policies are about whether *now* is the moment to remove something, not about what it
/// is, so they apply to an unanchored path exactly as to a proven one.
#[test]
fn should_hold_an_included_path_that_a_keep_policy_covers() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join("junk")).unwrap();
    fs::write(root.path().join("junk/leftovers.o"), b"output").unwrap();
    fs::write(
        root.path().join("voom.toml"),
        format!(
            "include = ['{}']\n\n[keep]\nmin_age = \"30d\"\n",
            root.path().join("junk").display()
        ),
    )
    .unwrap();

    let result = run(&options(root.path())).expect("the run completes");

    assert!(
        root.path().join("junk/leftovers.o").exists(),
        "a fresh directory is held by min_age however it was named"
    );
    assert!(result.entries.is_empty());
}

/// ADR 0006's central property, for the one path that reaches deletion without a marker.
#[test]
fn should_predict_an_unanchored_removal_in_a_dry_run() {
    let build = |root: &std::path::Path| {
        fs::create_dir_all(root.join("junk")).unwrap();
        fs::write(root.join("junk/leftovers.o"), b"output").unwrap();
        fs::write(
            root.join("voom.toml"),
            format!("include = ['{}']\n", root.join("junk").display()),
        )
        .unwrap();
    };

    let dry_root = TempDir::new().unwrap();
    build(dry_root.path());
    let before = snapshot(dry_root.path());
    let dry = run(&RunOptions {
        dry_run: true,
        ..options(dry_root.path())
    })
    .expect("the dry run completes");

    assert_eq!(snapshot(dry_root.path()), before, "a dry run changes nothing");

    let real_root = TempDir::new().unwrap();
    build(real_root.path());
    let real = run(&options(real_root.path())).expect("the run completes");

    let predicted: Vec<_> = dry
        .entries
        .iter()
        .map(|entry| entry.path.strip_prefix(dry_root.path()).unwrap().to_path_buf())
        .collect();
    let actual: Vec<_> = real
        .entries
        .iter()
        .map(|entry| entry.path.strip_prefix(real_root.path()).unwrap().to_path_buf())
        .collect();

    assert_eq!(predicted, actual, "the dry run predicts the real one exactly");
    assert!(!predicted.is_empty(), "and both actually found the included path");
}
