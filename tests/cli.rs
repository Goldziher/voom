//! The command-line surface: flags, output formats, and exit codes.
//!
//! Exit codes are the contract the published git hooks depend on (ADR 0009), so they are
//! asserted against the real binary rather than against the library.

// Helper functions here are not `#[test]` bodies, which is all `allow-expect-in-tests` covers.
// In a test crate a panic is the failure report, which is exactly what these want.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use support::snapshot;
use tempfile::TempDir;

/// A tree with one proven Rust artifact, one unproven `target/` holding a file nobody can
/// regenerate, and one opt-in `bin/`.
fn mixed_tree() -> TempDir {
    let root = TempDir::new().expect("a temporary directory");
    let path = root.path();
    fs::create_dir_all(path.join("proven/target/debug")).unwrap();
    fs::write(path.join("proven/Cargo.toml"), b"[package]").unwrap();
    fs::write(path.join("proven/target/debug/app"), vec![0u8; 4096]).unwrap();

    fs::create_dir_all(path.join("unproven/target")).unwrap();
    fs::write(path.join("unproven/target/notes.txt"), b"irreplaceable").unwrap();

    fs::create_dir_all(path.join("gotool/bin")).unwrap();
    fs::write(path.join("gotool/go.mod"), b"module example").unwrap();
    fs::write(path.join("gotool/bin/tool"), b"binary").unwrap();
    root
}

fn voom() -> Command {
    Command::cargo_bin("voom").expect("the binary builds")
}

#[test]
fn should_exit_zero_and_report_nothing_on_a_clean_tree() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("README.md"), b"nothing to do").unwrap();

    voom()
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("0 artifacts"));
}

/// The contract `.pre-commit-hooks.yaml` and `poly-hooks.toml` are committed against:
/// `--dry-run --report --exit-code` fails on findings and deletes nothing.
#[test]
fn should_exit_three_for_the_reporting_hook_when_there_are_findings() {
    let tree = mixed_tree();
    let before = snapshot(tree.path());

    voom()
        .args(["--dry-run", "--report", "--exit-code"])
        .arg(tree.path())
        .assert()
        .code(3);

    assert_eq!(
        snapshot(tree.path()),
        before,
        "the reporting hook must not touch the tree"
    );
}

#[test]
fn should_exit_zero_for_the_reporting_hook_when_there_is_nothing_to_find() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("README.md"), b"clean").unwrap();

    voom()
        .args(["--dry-run", "--report", "--exit-code"])
        .arg(root.path())
        .assert()
        .success();
}

#[test]
fn should_exit_two_on_a_usage_error() {
    voom()
        .args(["--min-age", "forever", "."])
        .assert()
        .code(2)
        .stderr(contains("not a duration"));
    voom()
        .args(["--ecosystem", "rustlang", "."])
        .assert()
        .code(2)
        .stderr(contains("rustlang"));
    voom().arg("/no/such/tree").assert().code(2);
}

#[test]
fn should_remove_only_what_a_marker_proves() {
    let tree = mixed_tree();
    voom().arg(tree.path()).assert().success();

    assert!(
        !tree.path().join("proven/target").exists(),
        "the proven artifact is swept"
    );
    assert!(
        tree.path().join("unproven/target/notes.txt").exists(),
        "unproven data survives"
    );
    assert!(
        tree.path().join("gotool/bin/tool").exists(),
        "an opt-in artifact survives"
    );
    assert!(tree.path().join("proven/Cargo.toml").exists(), "source survives");
}

#[test]
fn should_explain_every_skip_under_verbose() {
    let tree = mixed_tree();
    voom()
        .args(["--dry-run", "--verbose"])
        .arg(tree.path())
        .assert()
        .success()
        .stdout(contains("no rust marker"))
        .stdout(contains("not enabled"))
        .stdout(contains("go.bin"));
}

#[test]
fn should_hide_skip_detail_by_default_but_say_how_to_get_it() {
    let tree = mixed_tree();
    voom()
        .args(["--dry-run"])
        .arg(tree.path())
        .assert()
        .success()
        .stdout(contains("skipped"))
        .stdout(contains("--verbose for why"))
        .stdout(contains("no rust marker").not());
}

#[test]
fn should_emit_parseable_json_with_a_schema_version() {
    let tree = mixed_tree();
    let output = voom()
        .args(["--dry-run", "--format", "json"])
        .arg(tree.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let document: serde_json::Value = serde_json::from_slice(&output).expect("stdout parses as JSON");
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["dry_run"], true);
    assert_eq!(document["totals"]["reclaimed"], 1);
    let reasons: Vec<_> = document["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skip| skip["reason"].clone())
        .collect();
    assert!(reasons.contains(&serde_json::Value::from("no_marker")));
}

/// Diagnostics go to stderr and results to stdout, so `voom ~ --format json | jq` works
/// without filtering (ADR 0007).
#[test]
fn should_keep_json_on_stdout_free_of_diagnostics() {
    let tree = mixed_tree();
    let assert = voom()
        .args(["--dry-run", "--format", "json"])
        .arg(tree.path())
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "stdout is exactly one JSON document"
    );
}

#[test]
fn should_enable_an_opt_in_artifact_on_request() {
    let tree = mixed_tree();
    voom().args(["--enable", "go.bin"]).arg(tree.path()).assert().success();
    assert!(!tree.path().join("gotool/bin").exists());
}

#[test]
fn should_restrict_to_the_named_ecosystems() {
    let tree = mixed_tree();
    voom().args(["--ecosystem", "node"]).arg(tree.path()).assert().success();
    assert!(tree.path().join("proven/target").exists(), "rust was not selected");
}

#[test]
fn should_hold_everything_when_min_age_covers_it() {
    let tree = mixed_tree();
    let before = snapshot(tree.path());
    voom().args(["--min-age", "30d"]).arg(tree.path()).assert().success();
    assert_eq!(snapshot(tree.path()), before);
}

#[test]
fn should_print_only_the_footer_with_summary() {
    let tree = mixed_tree();
    voom()
        .args(["--dry-run", "--summary"])
        .arg(tree.path())
        .assert()
        .success()
        .stdout(contains("would remove").not())
        .stdout(contains("reclaimable"));
}

/// Tool caches are skipped by location, before classification, because an installed toolchain
/// and a built project are the same shape on disk (ADR 0001). Driven through the binary with a
/// fake `HOME`, which is process-scoped and so leaves other tests alone.
///
/// Unix only, and not because the behaviour is: `dirs::home_dir()` reads `$HOME` on Unix but
/// goes through the Shell known-folder API on Windows, so no environment variable can point it
/// at a fixture there. Faking a real profile directory would mean writing into the actual one.
/// The resolution this exercises takes `home` as an argument and is covered on every platform
/// by the unit tests in `src/caches.rs`; what is Unix-only here is the end-to-end wiring.
#[test]
#[cfg(unix)]
fn should_skip_a_tool_cache_under_home_and_sweep_it_with_the_flag() {
    let home = TempDir::new().unwrap();
    let cache = home.path().join(".npm/_cacache/pkg");
    fs::create_dir_all(cache.join("dist")).unwrap();
    fs::write(cache.join("package.json"), b"{}").unwrap();
    fs::write(cache.join("dist/bundle.js"), b"built").unwrap();

    let project = home.path().join("project");
    fs::create_dir_all(project.join("dist")).unwrap();
    fs::write(project.join("package.json"), b"{}").unwrap();
    fs::write(project.join("dist/bundle.js"), b"built").unwrap();

    voom()
        .env("HOME", home.path())
        .args(["--dry-run", "--format", "json"])
        .arg(home.path())
        .assert()
        .success()
        .stdout(contains("project/dist").and(contains("_cacache").not()));

    voom()
        .env("HOME", home.path())
        .args(["--dry-run", "--caches", "--format", "json"])
        .arg(home.path())
        .assert()
        .success()
        .stdout(contains("project/dist").and(contains("_cacache")));
}

/// Pointing voom *at* a cache is explicit intent. The skip is a rule about what a walk wanders
/// into, not about what the user asked for, so it must not need the flag.
///
/// Unix only for the reason above — and this one had to be gated rather than left alone,
/// because on Windows the fake `HOME` was ignored, no cache root resolved, and the assertion
/// passed without the skip logic ever running. A test that cannot fail is not a test.
#[test]
#[cfg(unix)]
fn should_sweep_a_cache_named_as_the_scan_root_without_the_flag() {
    let home = TempDir::new().unwrap();
    let cache = home.path().join(".npm/_cacache/pkg");
    fs::create_dir_all(cache.join("dist")).unwrap();
    fs::write(cache.join("package.json"), b"{}").unwrap();
    fs::write(cache.join("dist/bundle.js"), b"built").unwrap();

    voom()
        .env("HOME", home.path())
        .arg(home.path().join(".npm"))
        .assert()
        .success();

    assert!(!cache.join("dist").exists(), "a cache the user named outright is swept");
    assert!(cache.join("package.json").exists(), "and only its build output is");
}

#[test]
fn should_print_the_catalog() {
    voom()
        .arg("catalog")
        .assert()
        .success()
        .stdout(contains("Rust (rust)"))
        .stdout(contains("Cargo.toml"))
        .stdout(contains("node.build"));
}

#[test]
fn should_show_the_resolved_configuration_with_its_sources() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("voom.toml"), "[keep]\nmin_age = \"7d\"\n").unwrap();

    voom()
        .args(["config", "show"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("voom.toml"))
        .stdout(contains("command line"))
        .stdout(contains("min_age"));
}

#[test]
fn should_reject_a_broken_config_by_naming_the_file() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("voom.toml"), "[keep]\nmin_age = \"forever\"\n").unwrap();

    voom()
        .arg(root.path())
        .assert()
        .code(2)
        .stderr(contains("voom.toml"))
        .stderr(contains("forever"));
}

/// Two runs over an unchanged tree produce byte-identical output, which is what makes the
/// report diffable and therefore reviewable.
#[test]
fn should_produce_identical_output_across_two_dry_runs() {
    let tree = mixed_tree();
    let render = |tree: &Path| {
        let assert = voom()
            .args(["--dry-run", "--verbose", "--color", "never"])
            .arg(tree)
            .assert()
            .success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
        // The elapsed time is the one line that legitimately varies between runs.
        stdout
            .lines()
            .filter(|line| !line.contains(" in "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(render(tree.path()), render(tree.path()));
}

#[test]
fn should_suppress_color_when_asked() {
    let tree = mixed_tree();
    let assert = voom()
        .args(["--dry-run", "--color", "never"])
        .arg(tree.path())
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains('\u{1b}'), "no ANSI escapes when color is off");
}

/// The argument lists the two committed hook manifests actually publish.
///
/// Extracted from the files rather than retyped, because the point is that *those* files keep
/// working. A retyped copy would pass while the published manifest passed something voom no
/// longer accepts, which is the failure this guards. Extraction is a line scan rather than a
/// YAML and a TOML parser: both lines have a fixed shape, and the helper fails loudly if it
/// stops matching rather than quietly returning nothing.
fn published_hook_args(manifest: &str, marker: &str) -> Vec<String> {
    let line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with(marker))
        .unwrap_or_else(|| panic!("no line starting `{marker}` — the manifest format changed"));
    let list = line
        .split_once('[')
        .and_then(|(_, rest)| rest.rsplit_once(']'))
        .unwrap_or_else(|| panic!("no argument list in `{line}`"))
        .0;
    let args: Vec<String> = list
        .split(',')
        .map(|arg| arg.trim().trim_matches('"').to_owned())
        .filter(|arg| !arg.is_empty())
        .collect();
    assert!(!args.is_empty(), "no arguments parsed from `{line}`");
    args
}

/// ADR 0009's contract, against the manifests as committed: report, delete nothing, exit 3 so
/// the commit fails. These files are a release surface — consumers pin them by git revision —
/// so a flag voom stopped accepting would break every consumer at their next hook run, not
/// ours.
#[test]
fn should_honour_the_published_reporting_hook_arguments() {
    let manifests = [
        published_hook_args(include_str!("../.pre-commit-hooks.yaml"), "args:"),
        published_hook_args(include_str!("../poly-hooks.toml"), "args ="),
    ];

    for args in manifests {
        assert!(
            args.iter().any(|arg| arg == "--exit-code"),
            "the reporting hook must ask for the findings exit code: {args:?}"
        );

        let tree = mixed_tree();
        let before = snapshot(tree.path());
        voom().current_dir(tree.path()).args(&args).assert().code(3);
        assert_eq!(snapshot(tree.path()), before, "the reporting hook deletes nothing");

        let clean = TempDir::new().unwrap();
        fs::write(clean.path().join("README.md"), b"clean").unwrap();
        voom().current_dir(clean.path()).args(&args).assert().success();
    }
}

/// An unanchored removal has to be visible in the machine-readable output too, because a hook
/// consuming JSON is exactly the caller that cannot see the human column (ADR 0004).
#[test]
fn should_mark_an_unanchored_removal_in_json() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join("junk")).unwrap();
    fs::write(root.path().join("junk/leftovers.o"), b"output").unwrap();
    fs::write(
        root.path().join("voom.toml"),
        format!("include = ['{}']\n", root.path().join("junk").display()),
    )
    .unwrap();

    voom()
        .args(["--dry-run", "--format", "json"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("\"source\": \"config-include\"").and(contains("\"ecosystem\": null")));
}

/// `--one-file-system` is a safety control with three spellings, and the tri-state clap
/// configuration behind it is easy to break silently. Testing the crossing itself would need a
/// second filesystem; testing that every documented spelling parses does not, and a flag that
/// stops parsing is a flag that stops guarding.
#[test]
fn should_accept_every_spelling_of_the_filesystem_boundary_flag() {
    let tree = mixed_tree();
    for args in [
        vec!["--dry-run"],
        vec!["--dry-run", "--one-file-system"],
        vec!["--dry-run", "--one-file-system=true"],
        vec!["--dry-run", "--one-file-system=false"],
    ] {
        voom()
            .args(&args)
            .arg(tree.path())
            .assert()
            .success()
            .stdout(contains("would remove"));
    }
}
