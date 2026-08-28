//! Recursive size accounting.
//!
//! Each artifact is measured exactly once and the number feeds both the keep policies and the
//! report — never sized twice because the reporter wanted a number (ADR 0005). Artifacts are
//! independent, so measurement fans out over `rayon`.
//!
//! **The measured quantity is not the same on every platform.** On unix it is allocated blocks;
//! everywhere else, Windows included, it is apparent length, because no portable equivalent of
//! `blocks()` is available there. The two agree for ordinary files and diverge for sparse,
//! NTFS-compressed, or block-padded ones, where a Windows figure can overstate what the removal
//! actually frees (a compressed `target/`) or understate it (per-file slack). It is therefore a
//! good estimate rather than a promise on Windows, and `min_size`/`max_size` are evaluated
//! against the same figure the report prints, so a policy behaves consistently within a
//! platform even though the threshold means a slightly different thing across two.

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

/// Bytes occupied by one file, as reported by its length.
///
/// Off unix there is no portable allocated-size call, so this is the apparent size. See the
/// module documentation for what that changes.
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

    file_size(&metadata) + measure_dir(path, FANOUT_DEPTH)
}

/// How many levels of a tree are divided across `rayon` before the walk goes serial.
///
/// It bounds recursion depth, and that is the reason it exists rather than a tuning knob.
/// Recursing per directory reads well and overflows the stack: a 400-level chain — still inside
/// `PATH_MAX`, so an ordinary filesystem holds one — aborted the test suite on a `rayon`
/// worker's stack before this bound was added. Ten frames cannot.
///
/// Ten is also more than enough to divide the work. Branching even twice per level is a
/// thousand independent tasks, and a build tree reaches its wide directories (`target/debug/
/// deps/`, `build/`) within three or four.
const FANOUT_DEPTH: u32 = 10;

/// The recursive half of [`measure`], dividing each level across `rayon` until the bound.
///
/// Fanning out *within* one artifact, rather than only across artifacts, is what keeps sizing
/// off the critical path. A sweep's artifacts are wildly uneven — one `target/` was 79 GB of a
/// 127 GB total on the tree this was measured against — so `measure_all`'s per-artifact
/// parallelism left the largest tree to a single thread while every other core idled. Dividing
/// inside it took a `~/workspace` dry run from 6.88 s to 4.63 s (hyperfine, 8 runs).
fn measure_dir(dir: &Path, fanout: u32) -> u64 {
    let (total, subdirectories) = list(dir);
    if fanout == 0 {
        return total + subdirectories.iter().map(|path| measure_serially(path)).sum::<u64>();
    }
    total
        + match subdirectories.as_slice() {
            [] => 0,
            // Nothing to divide, and the frame still counts against the bound — a deep, narrow
            // chain is exactly the shape that would otherwise recurse without limit.
            [only] => measure_dir(only, fanout - 1),
            many => many.par_iter().map(|path| measure_dir(path, fanout - 1)).sum(),
        }
}

/// Everything below one directory, on one thread, with an explicit stack and no depth bound.
fn measure_serially(root: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let (bytes, subdirectories) = list(&dir);
        total += bytes;
        stack.extend(subdirectories);
    }
    total
}

/// One directory's own bytes, and the subdirectories below it.
///
/// An unreadable directory contributes nothing rather than failing the run: a size is an
/// estimate in a report, not a precondition for anything.
fn list(dir: &Path) -> (u64, Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, Vec::new());
    };
    let mut total = 0;
    let mut subdirectories = Vec::new();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata_no_follow() else {
            continue;
        };
        total += file_size(&metadata);
        if metadata.is_dir() {
            subdirectories.push(entry.path());
        }
    }
    (total, subdirectories)
}

/// Metadata for a directory entry, never following a link.
///
/// `DirEntry::metadata` is documented not to traverse a symlink, and on unix it is an `fstatat`
/// against the directory handle the walk already holds — where `symlink_metadata(entry.path())`
/// allocates a `PathBuf` and resolves the whole path from the root again. Over the 230,000
/// entries of an 89 GB `target/` that is about 0.4 µs an entry, and roughly 60 ms of a five
/// second sweep.
///
/// The trait survives the change so the guarantee stays named at the call site rather than
/// resting on a reader knowing what `DirEntry::metadata` does; `should_not_follow_a_symlink_when_measuring`
/// is what actually holds it, and it was checked against both routes.
trait NoFollow {
    fn metadata_no_follow(&self) -> std::io::Result<std::fs::Metadata>;
}

impl NoFollow for std::fs::DirEntry {
    fn metadata_no_follow(&self) -> std::io::Result<std::fs::Metadata> {
        self.metadata()
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

    /// The recursion runs on a `rayon` worker's stack rather than the main one, so a long chain
    /// has to return a number rather than overflow it.
    ///
    /// 400 levels is close to the real ceiling, and the ceiling is not the stack: single-letter
    /// components under a `TempDir` prefix put a deeper chain past `PATH_MAX`, which fails at
    /// `create_dir_all` with `File name too long`. The operating system bounds the depth far
    /// below anything a few hundred bytes of stack frame could exhaust.
    #[test]
    fn should_measure_a_chain_deeper_than_any_build_tool_produces() {
        let fixture = tempfile::TempDir::new().expect("a temp dir");
        let mut deep = fixture.path().to_path_buf();
        for _ in 0..400 {
            deep.push("d");
        }
        std::fs::create_dir_all(&deep).expect("a deep chain");
        std::fs::write(deep.join("leaf.o"), b"output").expect("a leaf file");

        assert!(
            measure(fixture.path()) > 0,
            "a 400-deep chain is measured rather than overflowing the stack"
        );
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

    /// The same rule one level down, where the walk rather than the entry point decides.
    ///
    /// [`measure`] stats its argument with `symlink_metadata`, so the test above holds however
    /// [`NoFollow`] is implemented. This one puts the link *inside* the tree being measured, so
    /// it is `metadata_no_follow` under test — and it fails if that ever becomes a following
    /// stat, which is the change that would quietly make every report promise space that
    /// removal will not reclaim.
    #[test]
    #[cfg(unix)]
    fn should_not_follow_a_symlink_found_inside_a_measured_tree() {
        let fixture = tree(&["heavy/one.bin", "heavy/two.bin", "heavy/three.bin", "target/app"]);
        let bare = measure(&fixture.path().join("target"));
        std::os::unix::fs::symlink(fixture.path().join("heavy"), fixture.path().join("target/link"))
            .expect("a symlink");

        let with_link = measure(&fixture.path().join("target"));

        assert!(
            with_link - bare < measure(&fixture.path().join("heavy")),
            "a link inside the tree added the weight of what it points at: {bare} -> {with_link}"
        );
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
