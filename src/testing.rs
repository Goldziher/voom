//! Fixture helpers for unit tests.
//!
//! The unit under test in this crate is "what does voom do to a filesystem", so nothing
//! smaller than a real directory tree is convincing. Every fixture lives under a [`TempDir`] —
//! a test that reached `$HOME`, or `/tmp` directly, would be a bug regardless of whether it
//! passed.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

/// Builds a fixture tree in a fresh temporary directory.
///
/// An entry ending in `/` becomes a directory; anything else becomes a file with placeholder
/// contents, and its parent directories are created for it.
///
/// # Panics
///
/// If the tree cannot be built, which means the test environment is broken.
#[must_use]
pub fn tree(entries: &[&str]) -> TempDir {
    let root = TempDir::new().expect("a temporary directory");
    build(root.path(), entries);
    root
}

/// Adds entries to an existing tree.
///
/// # Panics
///
/// If an entry cannot be created.
pub fn build(root: &Path, entries: &[&str]) {
    for entry in entries {
        let path = root.join(entry.trim_end_matches('/'));
        if entry.ends_with('/') {
            fs::create_dir_all(&path).expect("a fixture directory");
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("a fixture parent directory");
            }
            fs::write(&path, b"fixture").expect("a fixture file");
        }
    }
}

/// Every path under `root`, relative and sorted, for asserting on what a run left behind.
///
/// Directories carry a trailing `/` so a file and a directory of the same name are
/// distinguishable. Symlinks are recorded but never descended into.
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
