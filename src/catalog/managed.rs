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
    artifacts: &[Artifact::on("bin/"), Artifact::on("obj/")],
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
