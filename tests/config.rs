//! Configuration resolution, end to end.
//!
//! These drive the real pipeline over a fixture tree rather than the resolver in isolation,
//! because ADR 0004's subject is which file wins for which candidate — a question that only
//! has an answer once something is being classified at a path.
//!
//! They live here rather than in `src/run.rs` because they are integration tests by nature and
//! because `src/run.rs` was within twenty lines of the module cap.

// Helper functions here are not `#[test]` bodies, which is all `allow-expect-in-tests` covers.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::Path;
use std::time::Duration;

use support::{options, snapshot, tree};
use voom::classify::SkipReason;
use voom::config::resolve::Flags;
use voom::policy::KeepPolicy;
use voom::run::{RunOptions, run};

/// Writes a configuration file into a fixture tree.
fn write(root: &Path, relative: &str, contents: &str) {
    std::fs::write(root.join(relative), contents).expect("a config file");
}

/// Renders a path as a TOML *literal* string, in single quotes.
///
/// A Windows path in a TOML basic string is a parse error, not a path: `"C:\Users\..."` reads
/// `\U` as the start of a unicode escape. Literal strings take no escapes at all, which is what
/// a user on Windows has to write too.
fn toml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

/// The case the whole hierarchy exists for: a repository commits a `voom.toml` that
/// protects its own quirks, and it keeps working when someone sweeps from above.
#[test]
fn should_let_a_nested_config_protect_its_own_subtree() {
    let fixture = tree(&[
        "ordinary/Cargo.toml",
        "ordinary/target/app",
        "special/Cargo.toml",
        "special/target/app",
        "special/voom.toml",
    ]);
    write(
        fixture.path(),
        "special/voom.toml",
        "[ecosystems]\ndisable = [\"rust\"]\n",
    );

    let result = run(&options(fixture.path())).expect("the run completes");

    assert!(
        !fixture.path().join("ordinary/target").exists(),
        "the ordinary project is swept"
    );
    assert!(
        fixture.path().join("special/target/app").exists(),
        "the protected one is not"
    );
    assert_eq!(result.entries.len(), 1);
}

/// The mirror image: a subtree can opt *into* an artifact that is off by default, without
/// enabling it anywhere else.
#[test]
fn should_let_a_nested_config_enable_an_opt_in_artifact() {
    let fixture = tree(&[
        "ordinary/package.json",
        "ordinary/build/out.js",
        "eager/package.json",
        "eager/build/out.js",
        "eager/voom.toml",
    ]);
    write(
        fixture.path(),
        "eager/voom.toml",
        "[ecosystems]\nenable = [\"node.build\"]\n",
    );

    run(&options(fixture.path())).expect("the run completes");

    assert!(
        fixture.path().join("ordinary/build/out.js").exists(),
        "`build/` stays off by default"
    );
    assert!(
        !fixture.path().join("eager/build").exists(),
        "the subtree that opted in is swept"
    );
}

#[test]
fn should_apply_a_keep_policy_from_a_repository_config() {
    let fixture = tree(&["Cargo.toml", "target/app", "voom.toml"]);
    write(fixture.path(), "voom.toml", "[keep]\nmin_age = \"30d\"\n");
    let before = snapshot(fixture.path());

    let result = run(&options(fixture.path())).expect("the run completes");

    assert_eq!(snapshot(fixture.path()), before);
    assert!(matches!(
        result.skips[0].reason,
        SkipReason::KeptByPolicy { rule: "min_age", .. }
    ));
}

/// A per-ecosystem override narrows the base policy rather than replacing it.
#[test]
fn should_narrow_the_base_policy_with_a_per_ecosystem_override() {
    let fixture = tree(&["Cargo.toml", "target/app", "package.json", "dist/out.js", "voom.toml"]);
    write(
        fixture.path(),
        "voom.toml",
        "[keep]\nmin_age = \"30d\"\n\n[keep.rust]\nmin_age = \"0s\"\n",
    );

    run(&options(fixture.path())).expect("the run completes");

    assert!(!fixture.path().join("target").exists(), "rust's override releases it");
    assert!(
        fixture.path().join("dist/out.js").exists(),
        "node still inherits the 30d base"
    );
}

/// Flags sit above every configuration layer, at every depth.
#[test]
fn should_let_a_flag_override_a_nested_config() {
    let fixture = tree(&["repo/Cargo.toml", "repo/target/app", "repo/voom.toml"]);
    write(fixture.path(), "repo/voom.toml", "[ecosystems]\ndisable = [\"rust\"]\n");

    let mut options = options(fixture.path());
    options.flags.enable = vec!["rust.target".to_owned()];
    run(&options).expect("the run completes");

    assert!(!fixture.path().join("repo/target").exists(), "the command line wins");
}

#[test]
fn should_apply_a_path_rule_to_matching_artifacts_only() {
    let fixture = tree(&[
        "work/Cargo.toml",
        "work/target/app",
        "play/Cargo.toml",
        "play/target/app",
        "voom.toml",
    ]);
    let pattern = toml_path(&fixture.path().join("work").join("**"));
    write(
        fixture.path(),
        "voom.toml",
        &format!("[[paths]]\nmatch = {pattern}\nkeep = {{ min_age = \"30d\" }}\n"),
    );

    run(&options(fixture.path())).expect("the run completes");

    assert!(
        fixture.path().join("work/target/app").exists(),
        "the rule holds the matching tree"
    );
    assert!(
        !fixture.path().join("play/target").exists(),
        "and only the matching tree"
    );
}

/// `include` is the only documented way to sweep something no marker anchors. It was
/// parsed, merged and printed by `config show` while doing nothing at all.
#[test]
fn should_sweep_a_path_named_by_include_that_no_marker_proves() {
    let fixture = tree(&["junk/leftovers.o", "keep/leftovers.o", "voom.toml"]);
    write(fixture.path(), "voom.toml", "include = [\"junk\"]\n");

    let result = run(&options(fixture.path())).expect("the run completes");

    assert!(!fixture.path().join("junk").exists(), "the user named it outright");
    assert!(fixture.path().join("keep/leftovers.o").exists(), "and named only it");
    assert_eq!(result.entries.len(), 1);
    assert_eq!(
        result.entries[0].ecosystem(),
        None,
        "nothing proved it, and the report must not imply otherwise"
    );
}

/// The negative half: the same tree without the `include` line is left alone. This is the
/// test that would fail if `include` ever grew a default.
#[test]
fn should_leave_the_same_path_alone_without_the_include() {
    let fixture = tree(&["junk/leftovers.o", "voom.toml"]);
    write(fixture.path(), "voom.toml", "[keep]\nmin_age = \"0s\"\n");
    let before = snapshot(fixture.path());

    let result = run(&options(fixture.path())).expect("the run completes");

    assert!(result.entries.is_empty());
    assert_eq!(snapshot(fixture.path()), before);
}

/// An `include` naming a path outside the tree being swept is never walked, so it is never
/// removed. Containment holds without a special case (ADR 0006).
#[test]
fn should_not_reach_outside_the_scan_root_through_include() {
    let outside = tree(&["precious.txt"]);
    let fixture = tree(&["voom.toml"]);
    write(
        fixture.path(),
        "voom.toml",
        &format!("include = [{}]\n", toml_path(outside.path())),
    );

    let result = run(&options(fixture.path())).expect("the run completes");

    assert!(result.entries.is_empty());
    assert!(outside.path().join("precious.txt").exists());
}

#[test]
fn should_exclude_a_subtree_named_in_config() {
    let fixture = tree(&[
        "keep/Cargo.toml",
        "keep/target/app",
        "drop/Cargo.toml",
        "drop/target/app",
        "voom.toml",
    ]);
    let pattern = toml_path(&fixture.path().join("keep").join("**"));
    write(fixture.path(), "voom.toml", &format!("exclude = [{pattern}]\n"));

    run(&options(fixture.path())).expect("the run completes");

    assert!(fixture.path().join("keep/target/app").exists());
    assert!(!fixture.path().join("drop/target").exists());
}

/// `normalize_roots` walks the spelling the user typed rather than the canonical path
/// precisely so that this keeps working. Nothing pinned it until now.
#[test]
#[cfg(unix)]
fn should_keep_an_exclude_matching_when_the_root_is_reached_through_a_symlink() {
    let fixture = tree(&[
        "real/voom.toml",
        "real/keep/Cargo.toml",
        "real/keep/target/app",
        "real/drop/Cargo.toml",
        "real/drop/target/app",
    ]);
    let link = fixture.path().join("link");
    std::os::unix::fs::symlink(fixture.path().join("real"), &link).expect("a symlink");
    write(
        &link,
        "voom.toml",
        &format!("exclude = [{}]\n", toml_path(&link.join("keep").join("**"))),
    );

    let mut options = options(fixture.path());
    options.roots = vec![link.clone()];
    run(&options).expect("the run completes");

    assert!(
        fixture.path().join("real/keep/target/app").exists(),
        "an exclusion written against the typed spelling still has to match"
    );
    assert!(
        !fixture.path().join("real/drop/target").exists(),
        "and the rest of the tree is still swept"
    );
}

#[test]
fn should_reject_a_broken_config_and_name_the_file() {
    let fixture = tree(&["Cargo.toml", "target/app", "voom.toml"]);
    write(fixture.path(), "voom.toml", "[keep]\nmin_age = \"forever\"\n");

    let error = run(&options(fixture.path())).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("voom.toml"), "{message}");
    assert!(message.contains("forever"), "{message}");
    assert!(
        fixture.path().join("target/app").exists(),
        "nothing is removed when config is broken"
    );
}

/// `deny_unknown_fields` is a safety mechanism, not a strictness preference: a typo in a
/// key like `exclude` would otherwise leave the user with a guard they believe they have
/// and do not. ADR 0004 makes this explicit, and nothing pinned it.
#[test]
fn should_reject_an_unknown_key_rather_than_ignoring_it() {
    for broken in ["excludes = [\"x\"]\n", "[keep]\nmin_ages = \"7d\"\n"] {
        let fixture = tree(&["Cargo.toml", "target/app", "voom.toml"]);
        write(fixture.path(), "voom.toml", broken);

        let error = run(&options(fixture.path())).unwrap_err();
        assert!(
            error.to_string().contains("voom.toml"),
            "the file has to be named: {error}"
        );
        assert!(
            fixture.path().join("target/app").exists(),
            "and nothing is removed under a configuration voom could not read"
        );
    }
}

#[test]
fn should_reject_an_unknown_ecosystem_in_config() {
    let fixture = tree(&["Cargo.toml", "target/app", "voom.toml"]);
    write(fixture.path(), "voom.toml", "[ecosystems]\ndisable = [\"rustlang\"]\n");
    let error = run(&options(fixture.path())).unwrap_err();
    assert!(error.to_string().contains("rustlang"), "{error}");
}

#[test]
fn should_use_only_the_explicitly_named_config() {
    let fixture = tree(&["Cargo.toml", "target/app", "voom.toml", "other.toml"]);
    write(fixture.path(), "voom.toml", "[ecosystems]\ndisable = [\"rust\"]\n");
    write(fixture.path(), "other.toml", "");

    let mut options = options(fixture.path());
    options.config = Some(fixture.path().join("other.toml"));
    run(&options).expect("the run completes");

    assert!(
        !fixture.path().join("target").exists(),
        "--config replaces the discovered hierarchy"
    );
}

/// Nearest wins: a deeper file overrides a shallower one rather than merging blindly.
#[test]
fn should_let_the_nearest_config_win() {
    let fixture = tree(&["voom.toml", "repo/Cargo.toml", "repo/target/app", "repo/voom.toml"]);
    write(fixture.path(), "voom.toml", "[ecosystems]\ndisable = [\"rust\"]\n");
    write(
        fixture.path(),
        "repo/voom.toml",
        "[ecosystems]\nenable = [\"rust.target\"]\n",
    );

    run(&options(fixture.path())).expect("the run completes");

    assert!(!fixture.path().join("repo/target").exists(), "the nearer file wins");
}

/// `--min-age` is a blanket safety net, and a `voom.toml` in the tree must not be able to
/// release an artifact from it.
///
/// This is the case the module's own doc comment promises ("flags always win"), and the one an
/// operator relies on when sweeping an unfamiliar `$HOME` full of cloned repositories: any of
/// them may ship a committed `voom.toml`, and none of them should get to overrule the guard the
/// person at the keyboard asked for. Config-versus-config narrowing is a separate question, and
/// `should_narrow_the_base_policy_with_a_per_ecosystem_override` still pins it.
#[test]
fn should_not_let_a_per_ecosystem_override_release_an_artifact_a_flag_held() {
    let fixture = tree(&["Cargo.toml", "target/app", "voom.toml"]);
    write(fixture.path(), "voom.toml", "[keep.rust]\nmin_age = \"0s\"\n");
    let before = snapshot(fixture.path());

    let options = RunOptions {
        flags: Flags {
            keep: KeepPolicy {
                min_age: Some(Duration::from_secs(30 * 86_400)),
                ..KeepPolicy::default()
            },
            ..Flags::default()
        },
        ..options(fixture.path())
    };
    let result = run(&options).expect("the run completes");

    assert_eq!(
        snapshot(fixture.path()),
        before,
        "`--min-age 30d` holds the artifact; the repository's own config cannot release it"
    );
    assert!(matches!(
        result.skips[0].reason,
        SkipReason::KeptByPolicy { rule: "min_age", .. }
    ));
}

/// The same precedence, through a `[[paths]]` rule rather than a per-ecosystem table — the
/// other route by which configuration reaches the policy after the flags were folded in.
#[test]
fn should_not_let_a_path_rule_release_an_artifact_a_flag_held() {
    let fixture = tree(&["Cargo.toml", "target/app", "voom.toml"]);
    write(
        fixture.path(),
        "voom.toml",
        "[[paths]]\nmatch = \"**/target\"\nkeep = { min_age = \"0s\" }\n",
    );
    let before = snapshot(fixture.path());

    let options = RunOptions {
        flags: Flags {
            keep: KeepPolicy {
                min_age: Some(Duration::from_secs(30 * 86_400)),
                ..KeepPolicy::default()
            },
            ..Flags::default()
        },
        ..options(fixture.path())
    };
    run(&options).expect("the run completes");

    assert_eq!(snapshot(fixture.path()), before, "the flag still holds it");
}
