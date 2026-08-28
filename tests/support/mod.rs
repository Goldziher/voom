//! Fixture construction shared by the integration suites.
//!
//! Trees are built under a `TempDir` and never anywhere else. A test that touched `$HOME`, or
//! `/tmp` directly, would be a bug in a tool whose subject is what it does to a filesystem —
//! regardless of whether it passed.

// Each test binary compiles this module in full but uses only the helpers it needs, so
// unused-item warnings are an artefact of the shared-module pattern rather than dead code.
#![allow(dead_code)]
// A panic in fixture construction *is* the failure report. `allow-expect-in-tests` in
// clippy.toml only covers `#[test]` bodies, not helpers like these.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use voom::catalog::{Artifact, Ecosystem};

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
        // marker must sit, and the nearest place an `Ancestor` one may.
        fs::write(root.path().join(marker_name(ecosystem)), b"marker").expect("the marker file");
    }

    (root, target)
}

/// Every path under `root`, relative and sorted, for asserting on what survived.
///
/// # Panics
///
/// If a path cannot be rendered.
#[must_use]
pub fn snapshot(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    collect(root, root, &mut paths);
    paths.sort();
    paths
}

fn collect(root: &Path, dir: &Path, paths: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path).display().to_string();
        let file_type = entry.file_type();
        let is_symlink = file_type.as_ref().is_ok_and(std::fs::FileType::is_symlink);
        let is_dir = file_type.as_ref().is_ok_and(std::fs::FileType::is_dir);
        paths.push(if is_dir { format!("{relative}/") } else { relative });
        if is_dir && !is_symlink {
            collect(root, &path, paths);
        }
    }
}

/// Describes an entry for a failure message: `rust.target (target/)`.
#[must_use]
pub fn describe(ecosystem: &Ecosystem, artifact: &Artifact) -> String {
    format!("{}.{} ({})", ecosystem.id, artifact.slug(), artifact.path)
}
