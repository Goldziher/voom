//! Ecosystems on a managed runtime — the JVM and .NET.

use super::{Anchor, Artifact, Ecosystem};

pub(super) const MAVEN: Ecosystem = Ecosystem {
    id: "maven",
    name: "Maven",
    markers: &["pom.xml"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on("target/")],
};

pub(super) const GRADLE: Ecosystem = Ecosystem {
    id: "gradle",
    name: "Gradle / Kotlin",
    markers: &["build.gradle", "build.gradle.kts", "settings.gradle*"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on("build/"), Artifact::on(".gradle/")],
};

// .NET nests `bin/` and `obj/` inside a project whose `*.csproj` may be several directories
// up, so the marker is anchored at Ancestor(3) rather than as a sibling. Three is a bound on
// real project depth: too small misses artifacts, too large proves one project's `obj/` with
// an unrelated project's marker. See adrs/0003-builtin-catalog.md.
pub(super) const DOTNET: Ecosystem = Ecosystem {
    id: "dotnet",
    name: ".NET",
    markers: &["*.csproj", "*.fsproj", "*.vbproj", "*.sln"],
    anchor: Anchor::Ancestor(3),
    artifacts: &[
        // `bin/` anchors `Sibling`, not on the ecosystem's climb. A three-level climb let one
        // solution file license every `bin/` beneath it: `src/bin/` is Cargo's standard
        // multi-binary *source* layout, and `tools/scripts/bin/` is hand-written — both were
        // removed by default in a repository that also held a `.sln`. ADR 0003 records that a
        // sweep of a real machine found all 55 .NET artifacts at depth 0, so the climb never
        // bought recall here; it only widened what a single marker could reach.
        Artifact::on("bin/").at(Anchor::Sibling),
        // `obj/` keeps the climb. Nothing but MSBuild writes a directory of that name, so the
        // wider reach costs nothing.
        Artifact::on("obj/"),
    ],
};

pub(super) const SCALA: Ecosystem = Ecosystem {
    id: "scala",
    name: "Scala / sbt",
    markers: &["build.sbt"],
    anchor: Anchor::Sibling,
    artifacts: &[
        Artifact::on("target/"),
        Artifact::on("project/target/"),
        Artifact::on(".bloop/"),
        Artifact::on(".metals/"),
    ],
};
