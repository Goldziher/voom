//! Ruby and Dart.

use super::{Anchor, Artifact, Ecosystem};

pub(super) const RUBY: Ecosystem = Ecosystem {
    id: "ruby",
    name: "Ruby",
    markers: &["Gemfile", "*.gemspec"],
    anchor: Anchor::Sibling,
    artifacts: &[
        // NOTE: `.bundle/` is deliberately absent. It looks like a cache and it is not: it holds
        // `.bundle/config`, which `bundle config set --local` writes, which projects commit, and
        // which stores credentials for private gem sources. Nothing regenerates it. A `Gemfile`
        // proves Ruby; it does not prove that this directory is derived.
        //
        // `vendor/bundle/` is Ruby's *installed gem* directory inside `vendor/`, not `vendor/`
        // itself — ADR 0001 keeps the latter out of the catalog entirely. Off by default like
        // every other dependency install: restoring it costs a `bundle install` and a network,
        // which is the cost ADR 0001's amendment reserves `Artifact::off` for.
        Artifact::off(
            "vendor/bundle/",
            "A network-fetched dependency cache, not build output: removing it costs a full \
             `bundle install` and breaks offline work.",
        ),
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
