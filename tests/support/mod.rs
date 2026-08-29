//! Fixture construction shared by the integration suites.
//!
//! Trees are built under a `TempDir` and never anywhere else. A test that touched `$HOME`, or
//! `/tmp` directly, would be a bug in a tool whose subject is what it does to a filesystem —
//! regardless of whether it passed.

// Each test binary compiles this module in full but uses only the helpers it needs, so
// unused-item and unused-import warnings are an artefact of the shared-module pattern rather
// than dead code.
#![allow(dead_code, unused_imports)]
// A panic in fixture construction *is* the failure report. `allow-expect-in-tests` in
// clippy.toml only covers `#[test]` bodies, not helpers like these.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use voom::catalog::{Artifact, Ecosystem};
use voom::config::resolve::Flags;
use voom::run::RunOptions;

// One copy of the fixture-tree helpers, shared with the crate's unit tests through the
// `test-support` feature. Two copies drift, and a `snapshot` that handled symlinks or path
// separators differently here would have a safety test trusting output the unit suite could
// never have produced.
pub use voom::testing::{build, snapshot, tree};

/// A run over `root` with everything a suite normally wants: a real removal, deterministic
/// parallelism, and the skip reasons recorded so a failure can say why nothing matched.
///
/// A suite that needs one field different uses struct update — `RunOptions { dry_run: true,
/// ..options(root) }` — so the difference is the only thing on screen. A new field on
/// [`RunOptions`] is then one edit here rather than one per suite.
#[must_use]
pub fn options(root: &Path) -> RunOptions {
    RunOptions {
        roots: vec![root.to_path_buf()],
        dry_run: false,
        jobs: Some(2),
        one_file_system: true,
        force: false,
        caches: false,
        verbose: true,
        config: None,
        progress: false,
        flags: Flags::default(),
    }
}

/// [`options`] with `--force`, for the tests that are about what a retry reclaims.
#[must_use]
pub fn options_forcing(root: &Path) -> RunOptions {
    RunOptions {
        force: true,
        ..options(root)
    }
}

/// [`options`] with a set of `--enable` specs, for artifacts that are `default_on: false`.
#[must_use]
pub fn options_enabling(root: &Path, enable: Vec<String>) -> RunOptions {
    RunOptions {
        flags: Flags {
            enable,
            ..Flags::default()
        },
        ..options(root)
    }
}

/// Turns a catalog pattern into a concrete name a fixture can create.
///
/// `*.csproj` becomes `example.csproj`, `cmake-build-*` becomes `cmake-build-example`,
/// `bazel-*` becomes `bazel-example`. A literal is returned unchanged.
#[must_use]
pub fn concrete(pattern: &str) -> String {
    pattern.trim_end_matches('/').replace('*', "example")
}

/// The concrete relative path an artifact declaration produces.
#[must_use]
pub fn artifact_path(artifact: &Artifact) -> PathBuf {
    artifact.segments().map(concrete).collect()
}

/// The concrete name of the first marker that proves an ecosystem.
///
/// # Panics
///
/// If the ecosystem declares no marker — which the catalog invariants already forbid.
#[must_use]
pub fn marker_name(ecosystem: &Ecosystem) -> String {
    concrete(ecosystem.markers.first().expect("every ecosystem has a marker"))
}

/// Builds a tree containing an artifact, optionally with the marker that proves it.
///
/// Returns the root and the absolute path of the artifact. The artifact always contains a file
/// so that it has a non-zero size and so that a partial removal would be visible.
///
/// # Panics
///
/// If the tree cannot be created.
#[must_use]
pub fn fixture(ecosystem: &Ecosystem, artifact: &Artifact, with_marker: bool) -> (TempDir, PathBuf) {
    let root = TempDir::new().expect("a temporary directory");
    let relative = artifact_path(artifact);
    let target = root.path().join(&relative);

    if artifact.path.ends_with('/') {
        fs::create_dir_all(&target).expect("the artifact directory");
        fs::write(target.join("payload.bin"), b"build output").expect("artifact contents");
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("the artifact's parent");
        }
        fs::write(&target, b"build output").expect("the artifact file");
    }

    if with_marker {
        // The anchor is the artifact path with all of its own segments stripped off, which for
        // a fixture rooted at the artifact is always the root itself. That is where a `Sibling`
        // marker must sit, and the nearest place an `Ancestor` one may — but an `Inside` marker
        // must go *in* the artifact, and putting it at the root instead would make the positive
        // fixture silently assert the negative case.
        let marker_dir = if artifact.anchor_in(ecosystem).is_inside() {
            target.clone()
        } else {
            root.path().to_path_buf()
        };
        fs::write(marker_dir.join(marker_name(ecosystem)), b"marker").expect("the marker file");
    }

    (root, target)
}

/// Describes an entry for a failure message: `rust.target (target/)`.
#[must_use]
pub fn describe(ecosystem: &Ecosystem, artifact: &Artifact) -> String {
    format!("{}.{} ({})", ecosystem.id, artifact.slug(), artifact.path)
}
