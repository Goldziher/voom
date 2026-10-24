//! The cache-cleaning flags: listing, clearing, and the safety of both.
//!
//! Split from `tests/cli.rs` to keep each suite under the file-size limit; the exit codes and
//! output are asserted against the real binary there too. Every case plants caches under a fake
//! unix home, so the suite is unix-only.
#![cfg(unix)]
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
    let mut command = Command::cargo_bin("voom").expect("the binary builds");
    // The sweep runs Bazel housekeeping over the real machine's output roots unless told not to.
    command.env(voom::bazel::ROOTS_ENV, "");
    // Unlink in place: a detaching run would move fixtures into the real trash and start a
    // background reaper. Detached removal has its own suite in `tests/detach.rs`.
    command.env("VOOM_WAIT", "1");
    command
}

/// `--list-caches` lists only the caches present on this machine, each with its path, and exits
/// without ever scanning or measuring — a tree must stay untouched regardless of what it holds.
/// Driven with a fake `HOME`, so a machine that happens to lack, say, a Cargo registry does not
/// change what the command prints.
#[test]
fn should_list_only_present_caches_without_measuring_them() {
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
        // Sizing a machine's caches walks tens of gigabytes, so it is asked for with `--sizes`.
        .stdout(contains("MB").not())
        .stdout(contains("~/.cache/uv").not());

    assert_eq!(
        snapshot(tree.path()),
        before,
        "listing the cache table must not scan or remove"
    );
}

/// `--sizes` measures what the listing names, with the sweep's own sizer, so the figure is what
/// `--clean-caches` would reclaim.
#[test]
fn should_size_the_listing_when_asked() {
    let home = TempDir::new().unwrap();
    let registry = home.path().join(".cargo/registry");
    fs::create_dir_all(registry.join("src")).unwrap();
    fs::write(registry.join("CACHEDIR.TAG"), b"Signature: 8e477066c4a3e6a4\n").unwrap();
    fs::write(registry.join("src/lib.rs"), vec![0xA5; 1_500_000]).unwrap();

    voom()
        .env("HOME", home.path())
        .args(["--list-caches", "--sizes"])
        .assert()
        .success()
        .stdout(contains("~/.cargo/registry"))
        .stdout(contains("1.5"));

    voom()
        .env("HOME", home.path())
        .args(["--list-caches", "--sizes", "--verbose"])
        .assert()
        .success()
        .stdout(contains("1.5"));
}

/// `--verbose` prints the whole table — every known location with its state, the markers, and
/// what removing each cache costs — including the locations this machine does not have.
#[test]
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

/// Plants a tool cache the way its tool leaves it: the directory, and one proving marker.
fn plant_cache(home: &Path, location: &str, marker: &str) -> std::path::PathBuf {
    let dir = home.join(location);
    let proof = dir.join(marker);
    fs::create_dir_all(proof.parent().unwrap()).unwrap();
    fs::write(proof, b"x").unwrap();
    fs::write(dir.join("payload.bin"), vec![0xA5; 4096]).unwrap();
    dir
}

/// The bare flag and `=all` are documented as one request. Run side by side over the same home
/// they must agree on every path, and the audit that found one extra directory in `=all` was in
/// fact racing a concurrent build that created `target/` between the two runs.
#[test]
fn should_plan_the_same_removals_for_a_bare_flag_and_for_all() {
    let home = TempDir::new().unwrap();
    plant_cache(home.path(), ".cargo/registry", "CACHEDIR.TAG");
    plant_cache(home.path(), "Library/pnpm/store", "v11/index.db");
    plant_cache(home.path(), ".cache/huggingface/hub", ".locks");
    let tagged = home.path().join("work/relocated-target");
    fs::create_dir_all(&tagged).unwrap();
    fs::write(
        tagged.join("CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();

    let planned = |flag: &str| -> Vec<String> {
        let output = voom()
            .env("HOME", home.path())
            .args(["--dry-run", "--no-git", "--no-bazel", "--format", "json", flag])
            .arg(home.path())
            .output()
            .unwrap();
        let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut paths: Vec<String> = document["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|artifact| artifact["path"].as_str().unwrap().to_owned())
            .collect();
        paths.sort();
        paths
    };

    let bare = planned("--clean-caches");
    assert_eq!(bare, planned("--clean-caches=all"));
    assert!(bare.iter().any(|path| path.ends_with("relocated-target")), "{bare:?}");
    assert!(bare.iter().any(|path| path.ends_with("Library/pnpm/store")), "{bare:?}");
}

/// Models are gigabytes and sometimes gated, so the bare flag leaves them and only their id
/// removes them.
#[test]
fn should_remove_the_expensive_caches_only_when_named() {
    let home = TempDir::new().unwrap();
    let models = plant_cache(home.path(), ".cache/huggingface/hub", ".locks");
    let pip = plant_cache(home.path(), "Library/Caches/pip", "http-v2");

    voom()
        .env("HOME", home.path())
        .args(["--no-git", "--no-bazel", "--clean-caches"])
        .arg(home.path())
        .assert()
        .success();
    assert!(models.exists(), "a bare --clean-caches keeps the models");
    assert!(!pip.exists(), "and still removes the proven pip cache");

    voom()
        .env("HOME", home.path())
        .args(["--no-git", "--no-bazel", "--clean-caches=huggingface"])
        .arg(home.path())
        .assert()
        .success();
    assert!(!models.exists(), "naming it removes it");
}

/// pnpm's store sits under `Library/pnpm`, beside the global installs, so the walk must prune
/// the parent and still reach the store by name — and only the store.
#[test]
fn should_remove_the_pnpm_store_without_touching_the_global_installs() {
    let home = TempDir::new().unwrap();
    let store = plant_cache(home.path(), "Library/pnpm/store", "v11/index.db");
    let global = home.path().join("Library/pnpm/global/5/node_modules/tool");
    fs::create_dir_all(&global).unwrap();

    voom()
        .env("HOME", home.path())
        .args(["--no-git", "--no-bazel", "--clean-caches=pnpm-store"])
        .arg(home.path())
        .assert()
        .success();

    assert!(!store.exists());
    assert!(global.exists(), "the global installs beside the store are not a cache");
}

/// `--clean-caches=<ids>` removes only the named caches, so the command can stay explicit about
/// what survives when a run sweeps a real home directory.
#[test]
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

#[test]
fn should_include_relocated_tagged_builds_only_in_a_full_cache_clear() {
    for flag in [
        None,
        Some("--clean-caches"),
        Some("--clear-caches"),
        Some("--clean-caches=all"),
        Some("--clean-caches=cargo-registry"),
    ] {
        let home = TempDir::new().unwrap();
        let targets = [
            ".codex/targets/arbitrary-build",
            ".codex/automations/review/runs/issue/target",
            "repository/baseline-target",
            "repository/release-target",
        ];
        for relative in targets {
            let target = home.path().join(relative);
            fs::create_dir_all(target.join("debug/.fingerprint")).unwrap();
            fs::write(
                target.join("CACHEDIR.TAG"),
                b"Signature: 8a477f597d28d172789f06886806bc55\n",
            )
            .unwrap();
        }
        let invalid = home.path().join("saved-evidence");
        fs::create_dir_all(&invalid).unwrap();
        fs::write(invalid.join("CACHEDIR.TAG"), b"not a cache signature").unwrap();
        let installed = home.path().join(".rustup/toolchains/example");
        fs::create_dir_all(&installed).unwrap();
        fs::write(
            installed.join("CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        fs::write(home.path().join("voom.toml"), b"[tagged]\nenabled = false\n").unwrap();

        let mut command = voom();
        command.env("HOME", home.path()).args(["--no-bazel", "--no-git"]);
        if let Some(flag) = flag {
            command.arg(flag);
        }
        command.arg(home.path()).assert().success();

        let full_clear = flag.is_some_and(|flag| !flag.ends_with("cargo-registry"));
        for relative in targets {
            assert_eq!(home.path().join(relative).exists(), !full_clear, "{flag:?}: {relative}");
        }
        assert!(invalid.exists(), "an invalid tag never proves a directory");
        assert!(installed.exists(), "a full clear must not reopen installed toolchains");
    }
}

#[test]
fn should_require_worktree_cleanup_for_github_merge_lookups() {
    use clap::Parser;
    use voom::cli::{Cli, Command as CliCommand};

    for arguments in [
        vec!["voom", "--github-merged-worktrees"],
        vec!["voom", "git-prune", "--github-merged-worktrees"],
    ] {
        assert!(Cli::try_parse_from(arguments).is_err());
    }
    let sweep = Cli::try_parse_from(["voom", "--remove-merged-worktrees", "--github-merged-worktrees"])
        .expect("the sweep accepts both worktree options");
    assert!(sweep.prune.github_merged_worktrees);
    let prune = Cli::try_parse_from([
        "voom",
        "git-prune",
        "--remove-merged-worktrees",
        "--github-merged-worktrees",
    ])
    .expect("git-prune accepts both worktree options");
    let Some(CliCommand::GitPrune(prune)) = prune.command else {
        panic!("git-prune was parsed");
    };
    assert!(prune.github_merged_worktrees);
}

#[test]
fn should_accept_a_stale_age_for_worktrees_on_git_prune_only_with_worktree_cleanup() {
    use clap::Parser;
    use voom::cli::{Cli, Command as CliCommand};

    assert!(Cli::try_parse_from(["voom", "git-prune", "--stale-worktrees", "30d"]).is_err());
    let parsed = Cli::try_parse_from([
        "voom",
        "git-prune",
        "--remove-merged-worktrees",
        "--stale-worktrees",
        "30d",
    ])
    .expect("git-prune accepts a stale age with worktree cleanup");
    let Some(CliCommand::GitPrune(prune)) = parsed.command else {
        panic!("git-prune was parsed");
    };
    assert_eq!(prune.stale_worktrees.as_deref(), Some("30d"));
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

/// `all` carries the machine-wide Bazel clear, which stops servers other work may be building
/// with; `-bazel` takes exactly that back out and leaves the rest of the table.
#[test]
fn should_take_an_id_back_out_of_clean_caches_all() {
    let home = TempDir::new().unwrap();
    let pip = plant_cache(home.path(), "Library/Caches/pip", "http-v2");
    let bazel_root = home.path().join("_bazel_dev");
    let workspace = home.path().join("workspace");
    fs::create_dir_all(bazel_root.join("base/execroot")).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        bazel_root.join("base/DO_NOT_BUILD_HERE"),
        workspace.display().to_string(),
    )
    .unwrap();

    let planned = |flag: &str| -> serde_json::Value {
        let output = voom()
            .env("HOME", home.path())
            .env(voom::bazel::ROOTS_ENV, &bazel_root)
            .args(["--dry-run", "--no-git", "--format", "json", flag])
            .arg(home.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let has_pip = |document: &serde_json::Value| {
        document["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|artifact| artifact["path"].as_str() == pip.to_str())
    };
    let base_state = |document: &serde_json::Value| document["bazel"]["output_bases"][0]["state"].clone();

    let all = planned("--clean-caches=all");
    assert!(has_pip(&all));
    assert_eq!(base_state(&all), "cleared");

    let without = planned("--clean-caches=all,-bazel");
    assert!(has_pip(&without), "the rest of the table stays");
    assert_eq!(base_state(&without), "owned", "the live base is no longer cleared");
}

/// Go writes its module cache read-only on purpose; `go clean -modcache` restores the bits
/// before it unlinks, and so must voom, or the cache comes out part-way without `--force`.
#[test]
fn should_remove_a_read_only_module_cache_whole_without_force() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new().unwrap();
    let cache = plant_cache(home.path(), "go/pkg/mod", "cache");
    let module = cache.join("example.com/lib@v1.0.0");
    fs::create_dir_all(&module).unwrap();
    fs::write(module.join("lib.go"), "package lib\n").unwrap();
    fs::set_permissions(&module, fs::Permissions::from_mode(0o555)).unwrap();

    let output = voom()
        .env("HOME", home.path())
        .args(["--no-git", "--no-bazel", "--clean-caches=go-mod", "--format", "json"])
        .arg(home.path())
        .output()
        .unwrap();

    let survived = module.exists();
    if survived {
        fs::set_permissions(&module, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
    assert!(!survived, "the read-only module directory went with the cache");
    assert!(!cache.exists());
}
