//! Removal below narrow prefixes and branching subtrees.

use super::*;
use crate::testing::tree;

#[test]
fn should_remove_a_branching_tree_below_a_long_narrow_prefix() {
    let fixture = tree(&["artifact/", "source/precious"]);
    let artifact = fixture.path().join("artifact");
    let mut prefix = artifact.clone();
    for _ in 0..40 {
        prefix.push("narrow");
    }
    for index in 0..8 {
        let branch = prefix.join(index.to_string());
        std::fs::create_dir_all(&branch).unwrap();
        std::fs::write(branch.join("file"), [3u8; 4096]).unwrap();
    }
    std::os::unix::fs::symlink(fixture.path().join("source"), prefix.join("link")).unwrap();
    let expected = crate::size::measure(&artifact);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();

    let removed = pool.install(|| remove_dir(&artifact, false));

    assert!(
        removed.error.is_none(),
        "deep branching removal failed: {:?}",
        removed.error
    );
    assert_eq!(
        removed.freed, expected,
        "the prefix and branches must each be counted once"
    );
    assert!(!artifact.exists(), "the whole deep branching artifact must be removed");
    assert_eq!(
        std::fs::read(fixture.path().join("source/precious")).unwrap(),
        b"fixture"
    );
}
