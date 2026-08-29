//! Ecosystems that compile straight to native code and keep their output beside the manifest.

use super::{Anchor, Artifact, Ecosystem};

pub(super) const RUST: Ecosystem = Ecosystem {
    id: "rust",
    name: "Rust",
    markers: &["Cargo.toml"],
    anchor: Anchor::Sibling,
    artifacts: &[
        Artifact::on("target/"),
        Artifact::off(
            "vendor/",
            "A network-fetched dependency cache, not build output: removing it costs a full \
             `cargo vendor` and breaks offline builds.",
        ),
    ],
};

pub(super) const GO: Ecosystem = Ecosystem {
    id: "go",
    name: "Go",
    markers: &["go.mod"],
    anchor: Anchor::Sibling,
    artifacts: &[
        Artifact::off(
            "bin/",
            "`bin/` beside a go.mod is as often hand-written scripts as `go install` output.",
        ),
        Artifact::off(
            "vendor/",
            "A network-fetched dependency cache, not build output: removing it costs a full \
             `go mod vendor` and breaks offline and `-mod=vendor` builds.",
        ),
    ],
};

pub(super) const ZIG: Ecosystem = Ecosystem {
    id: "zig",
    name: "Zig",
    markers: &["build.zig"],
    anchor: Anchor::Sibling,
    artifacts: &[
        Artifact::on(".zig-cache/"),
        // Zig used an unhidden `zig-cache/` before 0.12. Both names are kept for recall; the
        // legacy one needs an explicit slug because it derives the same one as the current name.
        Artifact::on("zig-cache/").named("zig-cache-legacy"),
        Artifact::on("zig-out/"),
    ],
};

pub(super) const CMAKE: Ecosystem = Ecosystem {
    id: "cmake",
    name: "CMake / C++",
    markers: &["CMakeLists.txt"],
    anchor: Anchor::Sibling,
    artifacts: &[
        Artifact::on("build/"),
        Artifact::on("cmake-build-*/"),
        Artifact::on("CMakeFiles/"),
    ],
};

pub(super) const NIM: Ecosystem = Ecosystem {
    id: "nim",
    name: "Nim",
    markers: &["*.nimble"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on("nimcache/")],
};
