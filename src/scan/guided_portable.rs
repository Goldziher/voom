//! The index-guided walk's stand-in where directory link counts do not count entries: no work
//! tree is guided, and every one is walked by the directory walk.

use std::path::{Path, PathBuf};

pub(super) fn is_work_tree(_path: &Path) -> bool {
    false
}

pub(super) fn supported(_path: &Path) -> bool {
    false
}

pub(super) fn walk(
    _visitor: super::visitor::Visitor<'_>,
    work_trees: &[PathBuf],
    _sender: &std::sync::mpsc::Sender<super::Message>,
) -> Vec<PathBuf> {
    work_trees.to_vec()
}
