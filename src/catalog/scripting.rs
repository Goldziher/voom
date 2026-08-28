//! Ruby and Dart.

use super::{Anchor, Artifact, Ecosystem};

pub(super) const RUBY: Ecosystem = Ecosystem {
    id: "ruby",
    name: "Ruby",
    markers: &["Gemfile", "*.gemspec"],
    anchor: Anchor::Sibling,
    artifacts: &[
        Artifact::on(".bundle/"),
        // `vendor/bundle/` is Ruby's *installed gem* directory inside `vendor/`, not `vendor/`
        // itself — ADR 0001 keeps the latter out of the catalog entirely.
        Artifact::on("vendor/bundle/"),
        Artifact::on("coverage/"),
        Artifact::off(
            "pkg/",
            "`pkg/` also holds hand-maintained packaging input in many Ruby projects.",
        ),
        Artifact::off(
            "tmp/",
            "Rails regenerates most of `tmp/`, but applications also keep real data there.",
        ),
    ],
};

pub(super) const DART: Ecosystem = Ecosystem {
    id: "dart",
    name: "Dart / Flutter",
    markers: &["pubspec.yaml"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on(".dart_tool/"), Artifact::on("build/")],
};
