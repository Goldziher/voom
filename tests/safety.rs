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

use support::{options, options_enabling, snapshot};
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

/// A symlinked `target/` pointing at a source tree must not become a deletion of that tree —
/// and the user must be told the link is there, since one standing where an artifact belongs is
/// the shape of a mistake worth looking at.
///
/// The walker used to turn it into a skip, which put it inside an anonymous count unless `-v`
/// was passed. It is a finding now, refused by name.
#[test]
#[cfg(unix)]
fn should_never_delete_through_a_symlink() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Cargo.toml"), b"[package]").unwrap();
    fs::create_dir_all(root.path().join("source")).unwrap();
    fs::write(root.path().join("source/main.rs"), b"fn main() {}").unwrap();
    std::os::unix::fs::symlink(root.path().join("source"), root.path().join("target")).unwrap();

    let result = run(&options(root.path())).expect("the run completes");

    assert!(
        root.path().join("source/main.rs").exists(),
        "the symlink target survives"
    );
    assert!(root.path().join("target").is_symlink(), "the link itself survives");
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| (entry.path.file_name().and_then(std::ffi::OsStr::to_str), &entry.outcome))
            .collect::<Vec<_>>(),
        vec![(Some("target"), &Outcome::Refused(voom::delete::Refusal::Symlink))],
        "and the report names it as refused rather than counting it"
    );
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

    let result = run(&options(root.path())).expect("the run completes");

    assert!(
        outside.path().join("precious.txt").exists(),
        "the outside tree is untouched"
    );
    assert!(
        result
            .entries
            .iter()
            .all(|entry| matches!(entry.outcome, Outcome::Refused(voom::delete::Refusal::Symlink))),
        "and every entry the run produced was refused: {:?}",
        result.entries
    );
}

/// Ruby declares build output *inside* a dependency directory (`vendor/bundle/`), and the
/// walker reaches it by constructing that one path rather than walking the cache. A tree that
/// is both Ruby and PHP has one `vendor/` with two claims on it, and the PHP claim is off by
/// default — so the Ruby artifact must still be found.
///
/// This is a regression test with a date: the first `--clean-dependencies` implementation
/// classified the dependency directory and, on an artifact verdict, skipped the inside-check.
/// The classifier the pipeline uses is permissive, so "could be taken" was read as "will be
/// taken", and a default run silently stopped finding `vendor/bundle/`.
#[test]
fn should_still_find_build_output_inside_a_dependency_directory_another_ecosystem_claims() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Gemfile"), b"source 'https://rubygems.org'").unwrap();
    fs::write(root.path().join("composer.json"), b"{}").unwrap();
    fs::create_dir_all(root.path().join("vendor/bundle/gems")).unwrap();
    fs::write(root.path().join("vendor/bundle/gems/rake.rb"), b"gem").unwrap();
    fs::write(root.path().join("vendor/autoload.php"), b"<?php").unwrap();

    let result = run(&options(root.path())).expect("the run completes");

    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.path.strip_prefix(root.path()).unwrap_or(&entry.path).to_owned())
            .collect::<Vec<_>>(),
        vec![std::path::PathBuf::from("vendor").join("bundle")],
        "the Ruby artifact is found and the PHP dependency directory is not taken"
    );
    assert!(
        root.path().join("vendor/autoload.php").exists(),
        "and the rest of vendor/ is untouched"
    );
}

/// Enabling the outer dependency directory makes the artifact declared inside it redundant.
/// Removing both would count the inner one's bytes twice in the footer and then refuse it as
/// `Vanished`, which is a report that contradicts itself.
#[test]
fn should_not_remove_an_artifact_another_removal_already_covers() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Gemfile"), b"source 'https://rubygems.org'").unwrap();
    fs::write(root.path().join("composer.json"), b"{}").unwrap();
    fs::create_dir_all(root.path().join("vendor/bundle/gems")).unwrap();
    fs::write(root.path().join("vendor/bundle/gems/rake.rb"), b"gem").unwrap();

    let mut settings = options(root.path());
    settings.flags.enable.push("php.vendor".to_owned());
    let result = run(&settings).expect("the run completes");

    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.path.strip_prefix(root.path()).unwrap_or(&entry.path).to_owned())
            .collect::<Vec<_>>(),
        vec![std::path::PathBuf::from("vendor")],
        "only the covering artifact is removed"
    );
    assert!(!root.path().join("vendor").exists(), "and it really went");
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

/// Builds a `package.json` with a populated `node_modules/` beside it.
fn node_project() -> TempDir {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("package.json"), b"{}").unwrap();
    fs::create_dir_all(root.path().join("node_modules/left-pad")).unwrap();
    fs::write(
        root.path().join("node_modules/left-pad/index.js"),
        b"module.exports = 0",
    )
    .unwrap();
    root
}

/// **The data-loss regression test for `--clean-dependencies`.**
///
/// ADR 0001's amendment put dependency directories *into* the catalog, which means the thing
/// standing between a proven `node_modules/` and its deletion is now one `default_on` flag
/// rather than its absence from the table. A default run — the one every user gets, and the one
/// the README's "point it at `$HOME`" claim rests on — must leave it byte for byte alone.
///
/// Verified to fail: flipping the entry to `Artifact::on` deletes the tree here.
#[test]
fn should_never_remove_a_dependency_directory_by_default() {
    let root = node_project();
    let before = snapshot(root.path());

    let result = run(&options(root.path())).expect("the run completes");

    assert_eq!(
        snapshot(root.path()),
        before,
        "a default run changed a tree whose only removable thing is a dependency cache"
    );
    assert!(
        result.entries.is_empty(),
        "a dependency directory must not even be reported as removable without the opt-in"
    );
}

/// Nor does asking for some *other* opt-in reach it. `--enable` is per artifact, so an
/// unrelated spec must not drag the dependency group along with it.
#[test]
fn should_not_remove_a_dependency_directory_for_an_unrelated_opt_in() {
    let root = node_project();
    fs::create_dir_all(root.path().join("build")).unwrap();
    fs::write(root.path().join("build/out.js"), b"bundled").unwrap();

    let mut options = options(root.path());
    options.flags.enable = vec!["node.build".to_owned()];
    run(&options).expect("the run completes");

    assert!(!root.path().join("build").exists(), "the artifact asked for goes");
    assert!(
        root.path().join("node_modules/left-pad/index.js").exists(),
        "and the one that was not asked for stays"
    );
}

/// The opt-in itself, end to end. ADR 0001's amendment is what allows this at all, and it is
/// still marker-anchored: the `package.json` above is what proves the directory.
#[test]
fn should_remove_a_dependency_directory_when_it_is_explicitly_enabled() {
    let root = node_project();

    let mut options = options(root.path());
    options.flags.enable = vec!["node.node_modules".to_owned()];
    let result = run(&options).expect("the run completes");

    assert!(!root.path().join("node_modules").exists());
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].outcome, Outcome::Removed);
    assert!(root.path().join("package.json").exists(), "the source is untouched");
}

/// Enabling one member of the dependency group must not enable the rest — the group only
/// exists as `--clean-dependencies` sugar, not as a unit the classifier knows about.
#[test]
fn should_not_let_one_dependency_opt_in_enable_the_others() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("package.json"), b"{}").unwrap();
    fs::write(root.path().join("composer.json"), b"{}").unwrap();
    fs::write(root.path().join("mix.exs"), b"defmodule M do end").unwrap();
    for dependency in ["node_modules", "vendor", "deps"] {
        fs::create_dir_all(root.path().join(dependency)).unwrap();
        fs::write(root.path().join(dependency).join("payload"), b"fetched").unwrap();
    }

    let mut options = options(root.path());
    options.flags.enable = vec!["node.node_modules".to_owned()];
    run(&options).expect("the run completes");

    assert!(!root.path().join("node_modules").exists(), "the one named goes");
    assert!(root.path().join("vendor/payload").exists(), "composer's stays");
    assert!(root.path().join("deps/payload").exists(), "mix's stays");
}

/// `--clean-dependencies` is exactly the group, and nothing wider. A `.git/` in the same tree
/// is out of its reach for a different reason and stays that way.
#[test]
fn should_remove_the_whole_dependency_group_when_the_flag_asks_for_it() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("package.json"), b"{}").unwrap();
    fs::write(root.path().join("composer.json"), b"{}").unwrap();
    fs::write(root.path().join("pyproject.toml"), b"[project]").unwrap();
    for dependency in ["node_modules", "vendor", ".venv"] {
        fs::create_dir_all(root.path().join(dependency)).unwrap();
        fs::write(root.path().join(dependency).join("payload"), b"fetched").unwrap();
    }
    fs::create_dir_all(root.path().join(".git")).unwrap();
    fs::write(root.path().join(".git/HEAD"), b"ref: refs/heads/main").unwrap();

    let mut options = options(root.path());
    options.flags.enable = voom::catalog::DEPENDENCY_ARTIFACTS
        .iter()
        .map(|spec| (*spec).to_owned())
        .collect();
    run(&options).expect("the run completes");

    for dependency in ["node_modules", "vendor", ".venv"] {
        assert!(
            !root.path().join(dependency).exists(),
            "`{dependency}` survived the opt-in that names it"
        );
    }
    assert!(
        root.path().join(".git/HEAD").exists(),
        "a VCS directory is never reached"
    );
}

/// Ruby's `vendor/bundle/` is build output declared *inside* a dependency directory, and it is
/// on by default. Making `vendor/` itself an opt-in artifact must not have broken the targeted
/// check that reaches through the prune to find it.
#[test]
fn should_still_reach_build_output_inside_a_dependency_directory() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Gemfile"), b"source 'https://rubygems.org'").unwrap();
    fs::create_dir_all(root.path().join("vendor/bundle/gems")).unwrap();
    fs::write(root.path().join("vendor/bundle/gems/rake.rb"), b"# gem").unwrap();
    fs::create_dir_all(root.path().join("vendor/hand-written")).unwrap();
    fs::write(root.path().join("vendor/hand-written/keep.rb"), b"# mine").unwrap();

    let result = run(&options(root.path())).expect("the run completes");

    assert_eq!(result.entries.len(), 1);
    assert!(!root.path().join("vendor/bundle").exists(), "the gems go");
    assert!(
        root.path().join("vendor/hand-written/keep.rb").exists(),
        "and the rest of vendor/ is not touched by a default run"
    );
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

/// `Inside` proves a directory from its own contents, so the two ways it can be wrong are
/// symmetrical: taking one whose marker is only *beside* it, and passing over one whose marker
/// really is within. The second half is the data-loss case — a `.basemind` somebody wrote by
/// hand, next to a stray `agent-id` file, must survive even when the spec was asked for.
#[test]
fn should_prove_a_basemind_index_only_from_a_marker_inside_it() {
    let root = TempDir::new().unwrap();

    let proven = root.path().join("indexed/.basemind");
    fs::create_dir_all(&proven).unwrap();
    fs::write(proven.join("agent-id"), b"01").unwrap();
    fs::write(proven.join("blob"), b"index").unwrap();

    // The same marker name, one level out. This is the shape the generated fixtures cannot
    // produce, and the one a Sibling implementation would take.
    let beside = root.path().join("handwritten/.basemind");
    fs::create_dir_all(&beside).unwrap();
    fs::write(beside.join("notes.md"), b"mine").unwrap();
    fs::write(root.path().join("handwritten/agent-id"), b"01").unwrap();

    let asked = options_enabling(root.path(), vec!["basemind.basemind".to_owned()]);
    run(&asked).expect("the run completes");

    assert!(
        !proven.exists(),
        "a .basemind holding its own agent-id is proven, and this run asked for it"
    );
    assert!(
        beside.join("notes.md").exists(),
        "a .basemind proven only by a file beside it is not proven — this would be data loss"
    );
}

/// An index is rebuilt by re-reading and re-embedding a whole repository, so it comes out only
/// when someone says so. The `Inside` marker being present is not saying so.
#[test]
fn should_never_remove_a_basemind_index_without_being_asked() {
    let root = TempDir::new().unwrap();
    let index = root.path().join("indexed/.basemind");
    fs::create_dir_all(&index).unwrap();
    fs::write(index.join("agent-id"), b"01").unwrap();
    fs::write(index.join("blob"), b"index").unwrap();

    let result = run(&options(root.path())).expect("the run completes");

    assert!(
        index.join("blob").exists(),
        "a proven .basemind is still off by default — only an explicit spec reaches it"
    );
    assert!(result.entries.is_empty(), "and a default run must not report it either");
}

/// The `Ancestor(6)` climb alef needs, at the bound and one past it.
#[test]
fn should_sweep_an_alef_cache_within_its_ancestor_bound_and_stop_past_it() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("alef.toml"), b"[alef]").unwrap();

    for nesting in ["", "a/", "a/b/c/", "a/b/c/d/e/f/", "a/b/c/d/e/f/g/"] {
        let cache = root.path().join(format!("{nesting}.alef"));
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("snippet.bin"), b"output").unwrap();
    }

    run(&options(root.path())).expect("the run completes");

    for nesting in ["", "a/", "a/b/c/", "a/b/c/d/e/f/"] {
        assert!(
            !root.path().join(format!("{nesting}.alef")).exists(),
            ".alef/ at `{nesting}` is within Ancestor(6) and must be swept"
        );
    }
    assert!(
        root.path().join("a/b/c/d/e/f/g/.alef/snippet.bin").exists(),
        "seven levels down is beyond Ancestor(6) and nothing proves it"
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

/// A covered artifact is only covered if the removal that was supposed to take it happened.
///
/// Deciding coverage before the rails run left the inner artifact neither removed nor reported:
/// a skip line reading "already inside vendor" while `vendor/` was still sitting there, repeating
/// identically on every subsequent run. Here `vendor/` is read-only, so its own removal fails —
/// and `vendor/bundle/` must come back into the report rather than be written off.
#[test]
#[cfg(unix)]
fn should_not_write_off_a_covered_artifact_whose_coverer_was_never_removed() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Gemfile"), b"source 'https://rubygems.org'").unwrap();
    fs::write(root.path().join("composer.json"), b"{}").unwrap();
    // `vendor/bundle/` is empty, so the outer removal frees nothing before it fails: the case
    // under test is a coverer that never ran at all. A coverer that ran and freed something is
    // `PartiallyRemoved`, and its `freed` already counts the inner artifact — retrying that one
    // would report the same bytes twice.
    fs::create_dir_all(root.path().join("vendor/bundle")).unwrap();
    let vendor = root.path().join("vendor");
    fs::set_permissions(&vendor, fs::Permissions::from_mode(0o555)).unwrap();

    let mut settings = options(root.path());
    settings.verbose = true;
    settings.flags.enable.push("php.vendor".to_owned());
    let result = run(&settings).expect("the run completes");

    let named: Vec<String> = result
        .entries
        .iter()
        .map(|entry| {
            entry
                .path
                .strip_prefix(root.path())
                .unwrap_or(&entry.path)
                .display()
                .to_string()
        })
        .collect();
    fs::set_permissions(&vendor, fs::Permissions::from_mode(0o755)).unwrap();

    if named.len() == 1 && result.entries[0].outcome.is_reclaimed() {
        // Running as root defeats the permission bits, so `vendor/` really was removed and the
        // coverage claim is true. Nothing left to assert.
        return;
    }
    assert!(
        named.iter().any(|path| path.ends_with("bundle")),
        "the inner artifact is back in the report once its coverer was not removed, got {named:?}"
    );
    assert!(
        !result
            .skips
            .iter()
            .any(|skip| matches!(skip.reason, voom::classify::SkipReason::Covered { .. })),
        "and nothing claims it was taken by something else"
    );
}

/// A bare ecosystem id never widens to a dependency directory.
///
/// `--enable node` meant "turn on node's off-by-default build output", and before ADR 0001's
/// amendment there was nothing else it could mean. A `voom.toml` written against 0.1.0 —
/// `[ecosystems] enable = ["go"]`, to get `go.bin` — must not start deleting a checked-in
/// `vendor/` on upgrade. Those five are reachable only by their own spec.
#[test]
fn should_not_let_a_bare_ecosystem_enable_reach_a_dependency_directory() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("package.json"), b"{}").unwrap();
    fs::create_dir_all(root.path().join("node_modules/left-pad")).unwrap();
    fs::write(root.path().join("node_modules/left-pad/index.js"), b"module").unwrap();
    fs::create_dir_all(root.path().join("build")).unwrap();
    fs::write(root.path().join("build/bundle.js"), b"output").unwrap();

    let mut settings = options(root.path());
    settings.flags.enable.push("node".to_owned());
    let result = run(&settings).expect("the run completes");

    assert!(
        root.path().join("node_modules/left-pad/index.js").exists(),
        "a bare ecosystem id must not reach node_modules/"
    );
    assert!(
        !root.path().join("build").exists(),
        "but must still reach the off-by-default build output it has always meant: {:?}",
        result.entries
    );
}

/// `--disable` beats `--enable`, so the flag that says "not this" wins.
///
/// `--clean-dependencies` expands to `--enable node.node_modules` and friends, so with the
/// opposite order `voom --clean-dependencies --disable node` still removed `node_modules/`.
/// For a tool that deletes, the narrowing instruction has to be the one that survives.
#[test]
fn should_let_disable_beat_a_group_enable_on_the_command_line() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("package.json"), b"{}").unwrap();
    fs::create_dir_all(root.path().join("node_modules/left-pad")).unwrap();
    fs::write(root.path().join("node_modules/left-pad/index.js"), b"module").unwrap();

    let mut settings = options(root.path());
    settings.flags.enable.push("node.node_modules".to_owned());
    settings.flags.disable.push("node".to_owned());
    run(&settings).expect("the run completes");

    assert!(
        root.path().join("node_modules/left-pad/index.js").exists(),
        "--disable node must veto the group opt-in"
    );
}

/// A file whose name merely starts with `bazel-` is not build output.
///
/// The catalog's pattern had no trailing slash, so it matched files: a hand-written
/// `bazel-diff.sh` beside a `WORKSPACE` was classified and removed. The convenience symlinks the
/// entry exists for are still found — the walker classifies a link as though it were a
/// directory — and still refused.
#[test]
#[cfg(unix)]
fn should_not_remove_a_file_merely_named_like_a_bazel_symlink() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("WORKSPACE"), b"workspace(name = 'x')").unwrap();
    fs::write(root.path().join("bazel-diff.sh"), b"#!/bin/sh\necho hand written").unwrap();
    fs::write(root.path().join("bazel-env"), b"CONFIG=1").unwrap();

    let outside = TempDir::new().unwrap();
    fs::create_dir_all(outside.path().join("bin")).unwrap();
    fs::write(outside.path().join("bin/app"), b"output").unwrap();
    std::os::unix::fs::symlink(outside.path().join("bin"), root.path().join("bazel-bin")).unwrap();

    let result = run(&options(root.path())).expect("the run completes");

    assert!(
        root.path().join("bazel-diff.sh").exists(),
        "a script is not an artifact"
    );
    assert!(root.path().join("bazel-env").exists(), "nor is a config file");
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| (entry.path.file_name().and_then(std::ffi::OsStr::to_str), &entry.outcome))
            .collect::<Vec<_>>(),
        vec![(Some("bazel-bin"), &Outcome::Refused(voom::delete::Refusal::Symlink))],
        "and the convenience symlink is still found and refused"
    );
}

/// A coverer that ran and freed something already counted the inner artifact's bytes.
///
/// `PartiallyRemoved` is not `is_reclaimed`, so the covered artifact would otherwise be sent
/// through a second removal and reported at its *pre-removal* size — bytes the outer removal's
/// `freed` had already claimed. The footer would then say more was reclaimed than the disk gave
/// back, which is the defect this release exists to fix, running the other way.
#[test]
#[cfg(unix)]
fn should_not_count_a_covered_artifact_twice_when_its_coverer_partly_succeeded() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Gemfile"), b"source 'https://rubygems.org'").unwrap();
    fs::write(root.path().join("composer.json"), b"{}").unwrap();
    fs::create_dir_all(root.path().join("vendor/bundle/gems")).unwrap();
    fs::write(root.path().join("vendor/bundle/gems/rake.rb"), vec![0xA5; 64 * 1024]).unwrap();
    let vendor = root.path().join("vendor");
    fs::set_permissions(&vendor, fs::Permissions::from_mode(0o555)).unwrap();

    let mut settings = options(root.path());
    settings.flags.enable.push("php.vendor".to_owned());
    let result = run(&settings).expect("the run completes");
    fs::set_permissions(&vendor, fs::Permissions::from_mode(0o755)).unwrap();

    let partial = result
        .entries
        .iter()
        .any(|entry| matches!(entry.outcome, Outcome::PartiallyRemoved { .. }));
    if !partial {
        // Running as root, or a filesystem that let the whole removal through.
        return;
    }
    assert_eq!(
        result.entries.len(),
        1,
        "only the coverer is reported; the artifact inside it is not counted again: {:?}",
        result.entries.iter().map(|entry| &entry.path).collect::<Vec<_>>()
    );
}
