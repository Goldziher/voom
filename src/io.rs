//! Small filesystem helpers shared by the housekeeping paths.

use std::path::Path;

/// Reads a small control file, refusing anything that is not a regular file or is larger than
/// `max_bytes`.
///
/// A marker, a pid, or a state blob is small by construction, and the path often sits inside a
/// directory voom does not own. A FIFO at that path would block the read forever and a huge file
/// would exhaust memory, so both are refused rather than trusted. `symlink_metadata` also means
/// a symlink standing in for the file is not followed.
#[must_use]
pub(crate) fn read_capped_text(path: &Path, max_bytes: u64) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > max_bytes {
        return None;
    }
    std::fs::read_to_string(path).ok()
}
