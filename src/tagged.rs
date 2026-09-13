//! `CACHEDIR.TAG` detection: a directory that declares itself regenerable (ADR 0013).
//!
//! The [specification](https://bford.info/cachedir/) defines a file whose presence states that
//! the directory holds data the application can regenerate. `tar --exclude-caching`,
//! `rsync --exclude-tag`, Borg and restic all honour it without knowing anything about the tool
//! that wrote it, and ADR 0012 already argues the point voom needs:
//!
//! > A declaration a tool wrote into its own cache is *stronger* evidence than a file that
//! > happens to lie beside a directory.
//!
//! ADR 0012 spent that evidence only at *named locations*. ADR 0013 spends it on its own, which
//! is the one mechanism that can reach a `CARGO_TARGET_DIR` pointed at `/tmp` or a build
//! directory somebody renamed — neither of which any catalog entry can name.
//!
//! # Why the signature is verified here and not for a catalog marker
//!
//! The catalog matches markers by *name*, which is safe there because the name is only half the
//! evidence: `alef`'s `CACHEDIR.TAG` proves a candidate already called `.alef/`, and a cache
//! entry's proves one already at the tool's own path. This module has neither constraint — it
//! will remove a directory of any name anywhere under the scan root — so the name alone is not
//! enough. A file someone called `CACHEDIR.TAG` for their own reasons must not license the
//! removal of the directory holding it.

use std::io::Read;
use std::path::Path;

/// The tag file's name, fixed by the specification.
pub const TAG_FILE_NAME: &str = "CACHEDIR.TAG";

/// The exact first bytes a conforming tag begins with, per the specification.
const SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Whether `dir` carries a valid `CACHEDIR.TAG` and so declares itself regenerable.
///
/// Answers `false` for anything it cannot prove: no tag, an unreadable one, a short or
/// mis-signed one, or a tag that is not a regular file. Every one of those is the safe
/// direction — a false negative leaves a cache on disk, a false positive deletes a directory
/// nobody declared.
///
/// Costs one `symlink_metadata` per call, and a 43-byte read only for the rare directory that
/// has the file at all. It is never called unless the run asked for tagged directories.
#[must_use]
pub fn is_tagged(dir: &Path) -> bool {
    let tag = dir.join(TAG_FILE_NAME);
    // `symlink_metadata`, so a symlink named `CACHEDIR.TAG` cannot borrow a valid signature from
    // a real tag elsewhere and license this directory with it.
    if !std::fs::symlink_metadata(&tag).is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(&tag) else {
        return false;
    };
    let mut head = [0_u8; SIGNATURE.len()];
    // `read_exact` rather than `read`: a file shorter than the signature cannot carry it, and a
    // single `read` may legitimately return fewer bytes than asked for.
    file.read_exact(&mut head).is_ok() && head == *SIGNATURE
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn dir() -> TempDir {
        TempDir::new().expect("a temporary directory")
    }

    fn tag(at: &Path, body: &[u8]) {
        fs::write(at.join(TAG_FILE_NAME), body).expect("writing the tag");
    }

    #[test]
    fn should_accept_a_directory_carrying_the_exact_signature() {
        let root = dir();
        tag(root.path(), SIGNATURE);
        assert!(is_tagged(root.path()), "{} carries a valid tag", root.path().display());
    }

    #[test]
    fn should_accept_a_tag_with_the_usual_explanatory_body_after_the_signature() {
        let root = dir();
        let mut body = SIGNATURE.to_vec();
        body.extend_from_slice(b"\n# This directory contains a cache.\n");
        tag(root.path(), &body);
        assert!(is_tagged(root.path()), "the signature is a prefix, not the whole file");
    }

    #[test]
    fn should_refuse_a_directory_with_no_tag() {
        let root = dir();
        assert!(!is_tagged(root.path()), "no tag proves nothing");
    }

    #[test]
    fn should_refuse_a_tag_whose_signature_is_wrong() {
        let root = dir();
        tag(root.path(), b"Signature: 0000000000000000000000000000000000");
        assert!(
            !is_tagged(root.path()),
            "a file merely named CACHEDIR.TAG must not license a removal"
        );
    }

    #[test]
    fn should_refuse_a_tag_too_short_to_carry_the_signature() {
        let root = dir();
        tag(root.path(), b"Signature: 8a47");
        assert!(!is_tagged(root.path()), "a truncated tag is not a tag");
    }

    #[test]
    fn should_refuse_a_tag_that_is_a_directory() {
        let root = dir();
        fs::create_dir(root.path().join(TAG_FILE_NAME)).expect("creating the decoy");
        assert!(!is_tagged(root.path()), "only a regular file is a tag");
    }

    #[cfg(unix)]
    #[test]
    fn should_refuse_a_tag_that_is_a_symlink_to_a_real_one() {
        let root = dir();
        let elsewhere = dir();
        tag(elsewhere.path(), SIGNATURE);
        std::os::unix::fs::symlink(elsewhere.path().join(TAG_FILE_NAME), root.path().join(TAG_FILE_NAME))
            .expect("linking the tag");
        assert!(
            !is_tagged(root.path()),
            "a symlinked tag must not borrow another directory's declaration"
        );
    }

    #[test]
    fn should_refuse_a_directory_that_does_not_exist() {
        let root = dir();
        assert!(
            !is_tagged(&root.path().join("absent")),
            "an absent directory is not tagged"
        );
    }
}
