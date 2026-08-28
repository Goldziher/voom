//! Functional-language ecosystems.

use super::{Anchor, Artifact, Ecosystem};

pub(super) const ELIXIR: Ecosystem = Ecosystem {
    id: "elixir",
    name: "Elixir",
    markers: &["mix.exs"],
    anchor: Anchor::Sibling,
    artifacts: &[
        Artifact::on("_build/"),
        Artifact::on(".elixir_ls/"),
        Artifact::off(
            "deps/",
            "A network-fetched dependency cache, not build output: removing it costs a full \
             `mix deps.get` and breaks offline work.",
        ),
    ],
};

pub(super) const HASKELL: Ecosystem = Ecosystem {
    id: "haskell",
    name: "Haskell",
    markers: &["*.cabal", "stack.yaml"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on("dist-newstyle/"), Artifact::on(".stack-work/")],
};

pub(super) const OCAML: Ecosystem = Ecosystem {
    id: "ocaml",
    name: "OCaml / dune",
    markers: &["dune-project"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on("_build/")],
};
