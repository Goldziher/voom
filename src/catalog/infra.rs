//! Infrastructure tooling.

use super::{Anchor, Artifact, Ecosystem};

pub(super) const TERRAFORM: Ecosystem = Ecosystem {
    id: "terraform",
    name: "Terraform",
    markers: &["*.tf"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on(".terraform/")],
};

// NOTE: `bazel-bin` and friends are convenience *symlinks* into the output base. ADR 0006
// refuses to delete through a symlink, so these are found, reported, and refused rather than
// removed. That is the safe outcome; reclaiming the output base itself is out of v1 scope.
pub(super) const BAZEL: Ecosystem = Ecosystem {
    id: "bazel",
    name: "Bazel",
    markers: &["WORKSPACE", "WORKSPACE.bazel", "MODULE.bazel"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on("bazel-*")],
};
