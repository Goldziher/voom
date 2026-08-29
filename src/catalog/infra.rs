//! Infrastructure tooling.

use super::{Anchor, Artifact, Ecosystem};

// NOTE: `.terraform/` holds provider plugins and modules fetched from the registry, so it is
// fair to ask whether ADR 0001's "no dependency directories" rule covers it. It does not, and
// the distinction is deliberate: `terraform init` reconstructs it from the lock file in the
// working directory, it is per-workspace rather than shared, and duplicated provider binaries
// across a dozen module directories are the single largest reclaimable thing in most
// infrastructure repositories. What it never holds is resource state — that is `terraform.tfstate`
// beside it, or a remote backend. `.terraform/terraform.tfstate` records only which backend was
// initialised, and `init` writes it again. On by default, and confirmed against a real sweep.
pub(super) const TERRAFORM: Ecosystem = Ecosystem {
    id: "terraform",
    name: "Terraform",
    markers: &["*.tf"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on(".terraform/")],
};

// NOTE: `bazel-bin` and friends are convenience *symlinks* into the output base. ADR 0006
// refuses to delete through a symlink, so these are found, reported, and refused rather than
// removed. That is the safe outcome; reclaiming the output base itself is out of scope.
//
// The names are literal, not the `bazel-*/` glob this shipped with. Every genuine target here
// is a symlink and is refused, so the glob's only *effective* removals were false positives:
// a real `bazel-toolchains/` beside a WORKSPACE — the kind of thing people vendor or submodule
// — was removed by default. The entry exists to make the symlinks visible, and it cannot do
// that by matching things that are not them.
//
// `bazel-<workspace-name>` is the fourth convenience symlink and is deliberately absent: its
// name depends on the WORKSPACE's declared name, so no literal covers it and a glob is what
// caused the problem.
pub(super) const BAZEL: Ecosystem = Ecosystem {
    id: "bazel",
    name: "Bazel",
    markers: &["WORKSPACE", "WORKSPACE.bazel", "MODULE.bazel"],
    anchor: Anchor::Sibling,
    artifacts: &[
        Artifact::on("bazel-bin/"),
        Artifact::on("bazel-out/"),
        Artifact::on("bazel-testlogs/"),
    ],
};
