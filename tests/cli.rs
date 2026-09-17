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
    assert_eq!(document["schema_version"], voom::report::SCHEMA_VERSION);
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
fn should_skip_a_tool_cache_under_home() {
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
}

/// Pointing voom *at* a cache is explicit intent. The skip is a rule about what a walk wanders
/// into, not about what the user asked for, so naming the cache as the scan root must reach it.
///
/// Unix only for the reason above — and this one had to be gated rather than left alone,
/// because on Windows the fake `HOME` was ignored, no cache root resolved, and the assertion
/// passed without the skip logic ever running. A test that cannot fail is not a test.
#[test]
#[cfg(unix)]
fn should_sweep_a_cache_named_as_the_scan_root() {
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

/// A tree with a proven `node_modules/` and a `dist/` beside it.
fn node_tree() -> TempDir {
    let root = TempDir::new().expect("a temporary directory");
    fs::write(root.path().join("package.json"), b"{}").unwrap();
    fs::create_dir_all(root.path().join("node_modules/left-pad")).unwrap();
    fs::write(root.path().join("node_modules/left-pad/index.js"), b"module").unwrap();
    fs::create_dir_all(root.path().join("dist")).unwrap();
    fs::write(root.path().join("dist/bundle.js"), b"bundled").unwrap();
    root
}

/// The whole flag, through the real binary: without it the dependency cache is untouched.
#[test]
fn should_leave_node_modules_alone_without_the_dependency_flag() {
    let tree = node_tree();

    voom().arg(tree.path()).assert().success();

    assert!(
        tree.path().join("node_modules/left-pad/index.js").exists(),
        "an ordinary sweep must not reach a dependency cache"
    );
    assert!(!tree.path().join("dist").exists(), "and still sweeps build output");
}

#[test]
fn should_remove_node_modules_with_the_dependency_flag() {
    let tree = node_tree();

    voom().arg("--clean-dependencies").arg(tree.path()).assert().success();

    assert!(!tree.path().join("node_modules").exists());
    assert!(tree.path().join("package.json").exists(), "the source is untouched");
}

/// `--dry-run` is the same pipeline with the last step withheld, and that has to hold for the
/// one flag that reaches directories nothing else does.
#[test]
fn should_predict_a_dependency_removal_in_a_dry_run() {
    let tree = node_tree();
    let before = snapshot(tree.path());

    voom()
        .args(["--clean-dependencies", "--dry-run"])
        .arg(tree.path())
        .assert()
        .success()
        .stdout(contains("node_modules"));

    assert_eq!(snapshot(tree.path()), before, "a dry run must not touch the tree");
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
        // Wall-clock time is the only thing that legitimately varies between two runs over one
        // tree: the headline's total, and the stage breakdown under it. Both are dropped by
        // name rather than by a substring that an artifact path could also contain.
        stdout
            .lines()
            .filter(|line| !line.contains(" reclaimable in ") && !line.contains(" reclaimed in "))
            .filter(|line| !line.trim_start().starts_with("scan "))
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

/// Every `uvx` invocation the hook manifests publish must name the package and the command
/// separately.
///
/// `voom` was taken on `PyPI`, so the distribution is `voom-cli` while the command it installs is
/// `voom` (ADR 0010). `uvx voom-cli` therefore does not run voom — uv looks for a command
/// matching the package name, finds none, and refuses with a message pointing at `--from`. The
/// manifests pinned the short form, so the uvx channel of both published hooks would have
/// failed for every user on first run.
///
/// This asymmetry is permanent, which is why it is worth a test rather than a careful edit.
#[test]
fn should_invoke_uvx_with_the_package_named_separately_from_the_command() {
    let manifest = include_str!("../poly-hooks.toml");
    let invocations: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("uvx ") && (line.starts_with("run =") || line.starts_with("install =")))
        .collect();

    assert!(
        invocations.len() >= 2,
        "poly-hooks.toml publishes a uvx channel; the scan found {} invocations",
        invocations.len()
    );

    for line in invocations {
        assert!(
            line.contains("uvx --from voom-cli"),
            "`{line}` runs uvx without --from, so uv will look for a command named `voom-cli` \
             and fail; the package is voom-cli and the command is voom"
        );
    }
}

/// `--force` lives entirely after the rails, and `--dry-run` withholds the step it would act
/// on, so the two together have to be a strict no-op rather than a usage error — a hook alias
/// that carries both must keep working.
#[test]
fn should_accept_force_and_leave_a_dry_run_byte_identical() {
    let tree = mixed_tree();
    let before = support::snapshot(tree.path());

    voom()
        .args(["--dry-run", "--force"])
        .arg(tree.path())
        .assert()
        .success()
        .stdout(contains("would remove"));

    assert_eq!(
        support::snapshot(tree.path()),
        before,
        "a forced dry run changes nothing at all"
    );
}

/// `--list-caches` lists only the caches present on this machine, each with its path and size,
/// and exits without ever scanning — a tree must stay untouched regardless of what it holds.
/// Driven with a fake `HOME`, so a machine that happens to lack, say, a Cargo registry does not
/// change what the command prints.
#[test]
#[cfg(unix)]
fn should_list_only_present_caches_with_their_sizes() {
    let home = TempDir::new().unwrap();

    let registry = home.path().join(".cargo/registry");
    fs::create_dir_all(registry.join("src")).unwrap();
    fs::write(registry.join("CACHEDIR.TAG"), b"Signature: 8e477066c4a3e6a4\n").unwrap();
    fs::write(registry.join("src/lib.rs"), vec![0xA5; 1_500_000]).unwrap();

    // A present-but-unproven location is still on the machine, so it is listed too.
    let cacache = home.path().join(".npm/_cacache");
    fs::create_dir_all(&cacache).unwrap();
    fs::write(cacache.join("blob"), b"x").unwrap();

    let tree = mixed_tree();
    let before = snapshot(tree.path());

    voom()
        .env("HOME", home.path())
        .args(["--list-caches"])
        .arg(tree.path())
        .assert()
        .success()
        .stdout(contains("~/.cargo/registry"))
        .stdout(contains("~/.npm/_cacache"))
        // The registry holds ~1.5 MB, so the size column proves it measured rather than counted.
        .stdout(contains("1.5"))
        .stdout(contains("~/.cache/uv").not());

    assert_eq!(
        snapshot(tree.path()),
        before,
        "listing the cache table must not scan or remove"
    );
}

/// `--verbose` prints the whole table — every known location with its state, the markers, and
/// what removing each cache costs — including the locations this machine does not have.
#[test]
#[cfg(unix)]
fn should_list_the_whole_table_when_verbose() {
    let home = TempDir::new().unwrap();

    let registry = home.path().join(".cargo/registry");
    fs::create_dir_all(registry.join("src")).unwrap();
    fs::write(registry.join("CACHEDIR.TAG"), b"Signature: 8e477066c4a3e6a4\n").unwrap();
    fs::write(registry.join("src/lib.rs"), vec![0xA5; 1_500_000]).unwrap();

    let cacache = home.path().join(".npm/_cacache");
    fs::create_dir_all(&cacache).unwrap();
    fs::write(cacache.join("blob"), b"x").unwrap();

    voom()
        .env("HOME", home.path())
        .args(["--list-caches", "--verbose"])
        .assert()
        .success()
        .stdout(contains("Cargo registry"))
        .stdout(contains("(cargo-registry)"))
        .stdout(contains("~/.cargo/registry"))
        .stdout(contains("1.5"))
        .stdout(contains("present"))
        .stdout(contains("~/.npm/_cacache"))
        .stdout(contains("present, but no marker proves it"))
        .stdout(contains("~/.cache/uv"))
        .stdout(contains("not on this machine"))
        .stdout(contains("markers: CACHEDIR.TAG"));
}

/// A bare `--clean-caches` means "every cache the table knows about": both the proven survivors
/// go, each still on marker proof, and a location whose marker is absent is left alone. Driven
/// with a fake `HOME`, which is process-scoped and so leaves other tests alone.
#[test]
#[cfg(unix)]
fn should_clean_every_proven_cache_with_a_bare_flag() {
    let home = TempDir::new().unwrap();

    let registry = home.path().join(".cargo/registry");
    fs::create_dir_all(registry.join("src")).unwrap();
    fs::write(registry.join("CACHEDIR.TAG"), b"Signature: 8e477066c4a3e6a4\n").unwrap();

    let cacache = home.path().join(".npm/_cacache");
    fs::create_dir_all(&cacache).unwrap();
    fs::write(cacache.join("leftovers.o"), b"x").unwrap();

    voom()
        .env("HOME", home.path())
        .args(["--clean-caches"])
        .arg(home.path())
        .assert()
        .success();

    assert!(!registry.exists(), "a proven cache is removed by a bare --clean-caches");
    assert!(cacache.exists(), "an unproven cache is left alone");
}

/// `--clear-caches` is an alias for `--clean-caches`, because "clear" is how people reach for
/// it; the bare alias means the whole table, exactly like the spelled-out flag.
#[test]
#[cfg(unix)]
fn should_treat_clear_caches_as_an_alias_for_clean_caches() {
    let home = TempDir::new().unwrap();

    let registry = home.path().join(".cargo/registry");
    fs::create_dir_all(registry.join("src")).unwrap();
    fs::write(registry.join("CACHEDIR.TAG"), b"Signature: 8e477066c4a3e6a4\n").unwrap();

    voom()
        .env("HOME", home.path())
        .args(["--clear-caches"])
        .arg(home.path())
        .assert()
        .success();

    assert!(!registry.exists(), "a proven cache is removed by a bare --clear-caches");
}

/// An explicit `--clean-caches=all` is the same request as the bare flag, spelled out for a
/// script that wants to say what it means.
#[test]
#[cfg(unix)]
fn should_clean_every_proven_cache_with_an_explicit_all() {
    let home = TempDir::new().unwrap();

    for cache in [".cargo/registry", ".cargo/git"] {
        let dir = home.path().join(cache);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("CACHEDIR.TAG"), b"Signature: 8e477066c4a3e6a4\n").unwrap();
    }

    voom()
        .env("HOME", home.path())
        .args(["--clean-caches=all"])
        .arg(home.path())
        .assert()
        .success();

    assert!(
        !home.path().join(".cargo/registry").exists() && !home.path().join(".cargo/git").exists(),
        "--clean-caches=all removes every proven cache, exactly as the bare flag does"
    );
}

/// `--clean-caches=<ids>` removes only the named caches, so the command can stay explicit about
/// what survives when a run sweeps a real home directory.
#[test]
#[cfg(unix)]
fn should_clean_only_the_named_caches() {
    let home = TempDir::new().unwrap();

    let registry = home.path().join(".cargo/registry");
    fs::create_dir_all(registry.join("src")).unwrap();
    fs::write(registry.join("CACHEDIR.TAG"), b"Signature: 8e477066c4a3e6a4\n").unwrap();

    let cacache = home.path().join(".npm/_cacache/content-v2");
    fs::create_dir_all(&cacache).unwrap();
    fs::write(cacache.join("blob"), b"x").unwrap();

    voom()
        .env("HOME", home.path())
        .args(["--clean-caches=cargo-registry"])
        .arg(home.path())
        .assert()
        .success();

    assert!(!registry.exists(), "the named cache is removed");
    assert!(
        cacache.exists(),
        "a cache that was not named survives — including when it is proven"
    );
}

/// A mistyped id removes nothing and must fail rather than report success, which would be
/// indistinguishable from an empty cache.
#[test]
fn should_reject_an_unknown_cache_id() {
    let tree = mixed_tree();
    voom()
        .args(["--clean-caches=nope"])
        .arg(tree.path())
        .assert()
        .failure()
        .stderr(contains("not a known cache"));
    assert_eq!(snapshot(tree.path()), snapshot(tree.path()), "nothing was touched");
}
