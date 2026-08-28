//! Ecosystems rooted in a web toolchain.

use super::{Anchor, Artifact, Ecosystem};

pub(super) const NODE: Ecosystem = Ecosystem {
    id: "node",
    name: "Node / TypeScript",
    markers: &["package.json"],
    anchor: Anchor::Sibling,
    artifacts: &[
        Artifact::on("dist/"),
        Artifact::on(".next/"),
        Artifact::on(".nuxt/"),
        Artifact::on(".svelte-kit/"),
        Artifact::on(".astro/"),
        Artifact::on(".turbo/"),
        Artifact::on(".parcel-cache/"),
        Artifact::on(".vite/"),
        Artifact::on(".nyc_output/"),
        Artifact::on("*.tsbuildinfo"),
        Artifact::off(
            "build/",
            "`build/` beside a package.json is as often a hand-written build-script directory as output.",
        ),
    ],
};

pub(super) const PHP: Ecosystem = Ecosystem {
    id: "php",
    name: "PHP",
    markers: &["composer.json"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on(".phpunit.cache/"), Artifact::on(".phpunit.result.cache")],
};

pub(super) const ELM: Ecosystem = Ecosystem {
    id: "elm",
    name: "Elm",
    markers: &["elm.json"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on("elm-stuff/")],
};
