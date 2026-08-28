//! Scientific-computing ecosystems.

use super::{Anchor, Artifact, Ecosystem};

pub(super) const JULIA: Ecosystem = Ecosystem {
    id: "julia",
    name: "Julia",
    markers: &["Project.toml"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::off(
        "deps/build/",
        "Julia packages sometimes commit generated products under `deps/`.",
    )],
};

pub(super) const R: Ecosystem = Ecosystem {
    id: "r",
    name: "R",
    markers: &["DESCRIPTION"],
    anchor: Anchor::Sibling,
    artifacts: &[
        Artifact::on("*.Rcheck/"),
        Artifact::on("src/*.o"),
        Artifact::on("src/*.so"),
    ],
};
