//! The `CACHEDIR.TAG` route (ADR 0013): the properties CONTRIBUTING names as grounds for
//! rejecting a pull request.
//!
//! These tests are the specification. If one starts failing, the behaviour regressed — do not
//! "fix" the test. Each has been checked to actually fail when the property it guards is
//! removed.

// Helper functions here are not `#[test]` bodies, which is all `allow-expect-in-tests` covers.
// In a test crate a panic is the failure report, which is exactly what these want.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::fs;

use tempfile::TempDir;
use voom::run::{RunOptions, run};

// ---------------------------------------------------------------------------------------------
// ADR 0013 — a directory that declares itself regenerable with a `CACHEDIR.TAG`.
//
// The negative case below is the data-loss regression test: this is the only route that will
// remove a directory of *any* name, so the thing that must never break is its refusal to take
// one that did not declare itself.
// ---------------------------------------------------------------------------------------------

/// The case the catalog structurally cannot reach: a relocated `CARGO_TARGET_DIR` with no project
/// anywhere near it. Measured on one workstation, 111 GB of the disk was exactly this.
#[test]
fn should_remove_a_tagged_directory_no_marker_could_prove() {
    let root = TempDir::new().unwrap();
    support::tag(&root.path().join("enterprise-pro-target"));
    fs::write(root.path().join("enterprise-pro-target/payload.bin"), vec![0u8; 4096]).unwrap();

    let result = run(&support::options_tagged(root.path())).unwrap();

    assert!(
        !root.path().join("enterprise-pro-target").exists(),
        "a valid CACHEDIR.TAG is the declaration that proves it"
    );
    assert_eq!(result.entries.len(), 1, "exactly one finding: {:?}", result.entries);
}

/// uv tags every virtualenv it creates, and `uv tool install` puts each tool in one under
/// `~/.local/share/uv/tools/` — so the tag alone would uninstall working commands. A tagged
/// directory that is a virtualenv is not this route's to take.
#[test]
fn should_never_remove_a_tagged_virtualenv() {
    let root = TempDir::new().unwrap();
    let tool = root.path().join("uv/tools/acli");
    support::tag(&tool);
    fs::write(tool.join("pyvenv.cfg"), b"home = /usr/bin\n").unwrap();
    fs::create_dir_all(tool.join("bin")).unwrap();
    fs::write(tool.join("bin/acli"), b"#!/bin/sh\n").unwrap();

    run(&support::options_tagged(root.path())).unwrap();

    assert!(
        tool.join("bin/acli").exists(),
        "an installed tool survives a tagged sweep"
    );
}

/// **The data-loss regression test for ADR 0013.** An identically shaped directory that never
/// declared itself must survive, and must survive for the same reason every other negative
/// fixture does: nothing proved it.
#[test]
fn should_never_remove_an_untagged_directory() {
    let root = TempDir::new().unwrap();
    let untagged = root.path().join("enterprise-pro-target");
    fs::create_dir_all(&untagged).unwrap();
    fs::write(untagged.join("thesis.txt"), b"irreplaceable").unwrap();

    run(&support::options_tagged(root.path())).unwrap();

    assert!(untagged.join("thesis.txt").exists(), "no tag, no removal — ever");
}

/// A file merely *named* `CACHEDIR.TAG` is not a declaration. voom verifies the signature because
/// this route has neither a name nor a location constraining it.
#[test]
fn should_never_remove_a_directory_whose_tag_is_not_signed() {
    let root = TempDir::new().unwrap();
    let decoy = root.path().join("notes");
    fs::create_dir_all(&decoy).unwrap();
    fs::write(decoy.join("CACHEDIR.TAG"), b"my own notes about caching").unwrap();
    fs::write(decoy.join("thesis.txt"), b"irreplaceable").unwrap();

    run(&support::options_tagged(root.path())).unwrap();

    assert!(
        decoy.join("thesis.txt").exists(),
        "the signature is the proof, not the filename"
    );
}

/// Nothing happens without asking. A default sweep must not find a tagged directory, must not
/// report one, and must not remove one.
#[test]
fn should_never_remove_a_tagged_directory_by_default() {
    let root = TempDir::new().unwrap();
    support::tag(&root.path().join("relocated-target"));

    let result = run(&support::options(root.path())).unwrap();

    assert!(
        root.path().join("relocated-target").exists(),
        "a tagged directory is off by default (ADR 0013)"
    );
    assert!(
        result.entries.is_empty(),
        "and is not even reported: {:?}",
        result.entries
    );
}

/// **The performance property, and it is the reason this feature is not a cost.**
///
/// A tagged directory is an indivisible unit, so the walk must prune it rather than enumerate it.
/// Today, without this, voom descends the whole of a relocated 19 GB `CARGO_TARGET_DIR` and emits
/// thousands of "no marker proves it" skips to report nothing at all. Asserting on skips rather
/// than on findings is deliberate: `run.rs` drops a finding an outer one covers, so a nested
/// *finding* would disappear even if the walk had descended — the skips are what expose it.
#[test]
fn should_prune_a_tagged_directory_instead_of_walking_into_it() {
    let root = TempDir::new().unwrap();
    let tagged = root.path().join("relocated-target");
    support::tag(&tagged);
    // Each of these is a near-miss the classifier would report a skip for if it ever saw it.
    for nested in ["debug/build", "debug/deps/dist", "release/build", "release/obj"] {
        fs::create_dir_all(tagged.join(nested)).unwrap();
    }

    let result = run(&support::options_tagged(root.path())).unwrap();

    let inside: Vec<_> = result
        .skips
        .iter()
        .filter(|skip| skip.path.starts_with(&tagged))
        .map(|skip| skip.path.clone())
        .collect();
    assert!(
        inside.is_empty(),
        "the walk must not enumerate inside a tagged directory, but it reached: {inside:?}"
    );
    assert_eq!(
        result.skipped_count, 0,
        "and nothing below it was even counted as a candidate"
    );
}

/// A tag inside a tag is taken once, at the top. Nested tags are ordinary — Cargo tags a target
/// directory and the caches inside it — and counting both would double the reported bytes and
/// break ADR 0007's `sum(artifacts[].reclaimed_bytes) == totals.bytes`.
#[test]
fn should_take_the_outermost_tagged_directory_exactly_once() {
    let root = TempDir::new().unwrap();
    let outer = root.path().join("relocated-target");
    support::tag(&outer);
    support::tag(&outer.join("debug/incremental"));
    fs::write(outer.join("debug/incremental/payload.bin"), vec![0u8; 8192]).unwrap();

    let result = run(&support::options_tagged(root.path())).unwrap();

    assert_eq!(
        result.entries.len(),
        1,
        "the outer directory is an indivisible unit: {:?}",
        result.entries
    );
    assert!(!outer.exists(), "and it is gone");
    let totals = result.totals();
    let summed: u64 = result.entries.iter().map(voom::report::Entry::reclaimed_bytes).sum();
    assert_eq!(summed, totals.bytes, "ADR 0007's accounting invariant still holds");
}

/// An anchored artifact is the more specific answer and keeps the line. Cargo tags `target/`, but
/// a reader wants to be told it was Rust's, not that it carried a tag.
#[test]
fn should_report_an_anchored_artifact_as_its_ecosystem_even_when_tagged() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Cargo.toml"), b"[package]").unwrap();
    support::tag(&root.path().join("target"));

    let result = run(&support::options_tagged(root.path())).unwrap();

    assert_eq!(result.entries.len(), 1, "one finding: {:?}", result.entries);
    assert_eq!(
        result.entries[0].ecosystem(),
        Some("rust"),
        "the catalog is more specific than the tag and wins"
    );
}

/// Symlinks are never followed anywhere in voom, and probing `link/CACHEDIR.TAG` would resolve
/// through the link. So a symlinked tagged directory is passed over and its target survives.
#[cfg(unix)]
#[test]
fn should_refuse_a_symlinked_tagged_directory_and_leave_its_target() {
    let root = TempDir::new().unwrap();
    let real = TempDir::new().unwrap();
    support::tag(real.path());
    fs::write(real.path().join("thesis.txt"), b"irreplaceable").unwrap();
    std::os::unix::fs::symlink(real.path(), root.path().join("relocated-target")).unwrap();

    run(&support::options_tagged(root.path())).unwrap();

    assert!(
        real.path().join("thesis.txt").exists(),
        "a symlinked tagged directory must never become a deletion of its target"
    );
}

/// voom never removes the tree it was pointed at, tagged or not — containment demands strictly
/// below a scan root (ADR 0006).
#[test]
fn should_refuse_a_tagged_directory_that_is_the_scan_root() {
    let root = TempDir::new().unwrap();
    support::tag(root.path());
    fs::write(root.path().join("thesis.txt"), b"irreplaceable").unwrap();

    run(&support::options_tagged(root.path())).unwrap();

    assert!(
        root.path().join("thesis.txt").exists(),
        "the scan root is never its own removal target"
    );
}

/// `--dry-run` is the same pipeline with the last step withheld, and that has to hold for the
/// newest source too.
#[test]
fn should_leave_a_tagged_tree_byte_identical_under_dry_run() {
    let root = TempDir::new().unwrap();
    let tagged = root.path().join("relocated-target");
    support::tag(&tagged);
    fs::write(tagged.join("payload.bin"), vec![7u8; 4096]).unwrap();

    let options = RunOptions {
        dry_run: true,
        ..support::options_tagged(root.path())
    };
    let result = run(&options).unwrap();

    assert_eq!(result.entries.len(), 1, "it is found: {:?}", result.entries);
    assert!(tagged.join("payload.bin").exists(), "and nothing was touched");
    assert_eq!(
        fs::read(tagged.join("payload.bin")).unwrap(),
        vec![7u8; 4096],
        "byte-identical"
    );
}

/// The shape the generated fixtures cannot produce, and the one that cost 18.86 GB in the field.
///
/// `Inside` means the marker sits *directly* inside the candidate. `tests/catalog_fixtures.rs`
/// always places it there, so an entry whose tool tags a **subdirectory** instead looks identical
/// to a directory that was never that tool's — which is exactly what alef did, leaving 82 of 83
/// `.alef/` directories unproven and silently unswept (see the correction to ADR 0012).
///
/// This pins the boundary rather than moving it: the anchor is not loosened to go looking one level
/// down, because a marker high in a tree must never license everything beneath it. The remedy is
/// for the tool to tag its own root, and `--clean-tagged` is the fallback that reaches the tagged
/// subdirectory meanwhile.
#[test]
fn should_not_prove_an_inside_anchor_from_a_marker_one_level_deeper() {
    let root = TempDir::new().unwrap();
    let alef = root.path().join("project/.alef");
    support::tag(&alef.join("snippets"));
    fs::write(alef.join("snippets/payload.bin"), vec![0u8; 4096]).unwrap();

    run(&support::options(root.path())).unwrap();

    assert!(
        alef.join("snippets/payload.bin").exists(),
        "a tag in `.alef/snippets/` does not prove `.alef/` — the anchor means *directly* inside"
    );
}

/// The other half: with ADR 0013's opt-in, the directory that *did* declare itself is reached, even
/// though its parent is still unproven. This is the escape hatch for a tool that has not yet
/// tagged its own root.
#[test]
fn should_reach_a_tagged_subdirectory_of_an_unproven_parent() {
    let root = TempDir::new().unwrap();
    let alef = root.path().join("project/.alef");
    support::tag(&alef.join("snippets"));
    fs::write(alef.join("snippets/payload.bin"), vec![0u8; 4096]).unwrap();

    run(&support::options_tagged(root.path())).unwrap();

    assert!(
        !alef.join("snippets").exists(),
        "the tagged subdirectory declared itself"
    );
    assert!(alef.exists(), "and its unproven parent is untouched");
}

/// The flag is off by default, and silence is the trap: a tree of relocated build output reports
/// `0 artifacts` with nothing saying one flag would reach it. A verbose run names it.
#[test]
fn should_point_at_a_tagged_directory_it_was_not_asked_to_remove() {
    let root = TempDir::new().unwrap();
    let relocated = root.path().join("xberg-enterprise-target-20261030");
    support::tag(&relocated);
    fs::write(relocated.join("payload.bin"), vec![0u8; 4096]).unwrap();

    let result = run(&support::options(root.path())).unwrap();

    assert!(relocated.exists(), "without the flag a tagged directory is left alone");
    assert!(
        result
            .skips
            .iter()
            .any(|skip| skip.reason.code() == "tagged_not_enabled"),
        "a verbose run names the flag that reaches it: {:?}",
        result.skips
    );
}
