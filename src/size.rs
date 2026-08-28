//! Recursive size accounting.
//!
//! Each artifact is measured exactly once and the number feeds both the keep policies and the
//! report — never sized twice because the reporter wanted a number (ADR 0005). Artifacts are
//! independent, so measurement fans out over `rayon`.

use std::path::{Path, PathBuf};

use rayon::prelude::*;

/// Bytes actually occupied by one file.
///
/// On Unix this is allocated blocks rather than apparent length, because the headline claim is
/// about space *reclaimed* and a sparse or block-padded file reclaims what it occupies, not
/// what it reports as its length.
#[cfg(unix)]
fn file_size(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks() * 512
}

#[cfg(not(unix))]
fn file_size(metadata: &std::fs::Metadata) -> u64 {
    metadata.len()
}

/// The recursive size of a path, in bytes.
///
/// Symlinks are measured as links, never followed — the same rule the walker and the deleter
/// use, so the number matches what removal will actually reclaim. Unreadable subtrees
/// contribute what could be read rather than aborting; a size is an estimate in a report, not
/// a precondition for anything.
#[must_use]
pub fn measure(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return file_size(&metadata);
    }

    let mut total = file_size(&metadata);
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata_no_follow() else {
                continue;
            };
            total += file_size(&metadata);
            if metadata.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    total
}

/// `DirEntry::metadata` follows symlinks on some platforms; this never does.
trait NoFollow {
    fn metadata_no_follow(&self) -> std::io::Result<std::fs::Metadata>;
}

impl NoFollow for std::fs::DirEntry {
    fn metadata_no_follow(&self) -> std::io::Result<std::fs::Metadata> {
        std::fs::symlink_metadata(self.path())
    }
}

/// Measures many paths concurrently.
#[must_use]
pub fn measure_all(paths: &[PathBuf]) -> Vec<u64> {
    paths.par_iter().map(|path| measure(path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tree;

    #[test]
    fn should_measure_a_file() {
        let fixture = tree(&["file.txt"]);
        assert!(measure(&fixture.path().join("file.txt")) > 0);
    }

    #[test]
    fn should_measure_a_directory_recursively() {
        let fixture = tree(&["target/a.o", "target/deep/b.o", "target/deep/deeper/c.o"]);
        let shallow = tree(&["target/a.o"]);
        assert!(
            measure(&fixture.path().join("target")) > measure(&shallow.path().join("target")),
            "a deeper tree must measure larger"
        );
    }

    #[test]
    fn should_return_zero_for_a_path_that_does_not_exist() {
        let fixture = tree(&["Cargo.toml"]);
        assert_eq!(measure(&fixture.path().join("nothing")), 0);
    }

    /// A symlinked directory is measured as the link, not as what it points at — otherwise the
    /// report would promise to reclaim space that removal will not touch.
    #[test]
    #[cfg(unix)]
    fn should_not_follow_a_symlink_when_measuring() {
        let fixture = tree(&["big/one.bin", "big/two.bin", "big/three.bin"]);
        std::os::unix::fs::symlink(fixture.path().join("big"), fixture.path().join("link")).expect("a symlink");
        assert!(measure(&fixture.path().join("link")) < measure(&fixture.path().join("big")));
    }

    #[test]
    fn should_measure_many_paths_in_one_pass() {
        let fixture = tree(&["a/x.o", "b/y.o"]);
        let paths = vec![fixture.path().join("a"), fixture.path().join("b")];
        let sizes = measure_all(&paths);
        assert_eq!(sizes.len(), 2);
        assert!(sizes.iter().all(|size| *size > 0));
    }
}
