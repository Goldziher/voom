//! The built-in ecosystem catalog: the table that says which directories are build output,
//! and what proves it.
//!
//! This is a `const` table compiled into the binary rather than a file loaded at runtime, so
//! the catalog is part of the tested, versioned artifact and startup costs no I/O. See
//! `adrs/0003-builtin-catalog.md`.
//!
//! Every entry is a licence to delete a directory. The invariants at the bottom of this file
//! are what keep that safe, and they are asserted over the whole table at test time rather
//! than left to review attention.

mod apple;
mod functional;
mod infra;
mod managed;
mod native;
mod python;
mod scientific;
mod scripting;
mod tooling;
mod web;

/// Where a marker file may sit relative to an artifact's anchor directory.
///
/// The anchor directory is the candidate path with the artifact's own relative path stripped
/// off: for `target/` anchored at `Sibling`, the anchor is `target/`'s parent, and the marker
/// must be in it. `Ancestor(n)` additionally allows the marker to be up to `n` levels above
/// the anchor, which is what ecosystems that nest artifacts inside a project need — .NET's
/// `obj/` is proven by a `*.csproj` that may be several directories up.
///
/// `n` is a bound, not a suggestion: too small misses artifacts, too large risks proving one
/// project's artifact with an unrelated project's marker.
///
/// [`Inside`](Anchor::Inside) looks in the candidate directory itself rather than around it,
/// which is what a tool cache needs: nothing beside `~/.cargo/registry` says what it is, but
/// the `CACHEDIR.TAG` cargo wrote *into* it does. That is stronger proof than a sibling, not
/// weaker — the tool declared the directory regenerable itself — and it costs one `read_dir`
/// of a directory the walker prunes and never enumerates anyway.
///
/// `#[non_exhaustive]` because a fourth position is plausible and adding one to a bare enum
/// would break every downstream `match` without a wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Anchor {
    /// The marker sits in the anchor directory itself.
    Sibling,
    /// The marker sits in the anchor directory or up to `n` levels above it.
    Ancestor(u8),
    /// The marker sits inside the candidate directory.
    Inside,
}

impl Anchor {
    /// How many levels above the anchor directory the marker search may climb.
    ///
    /// Zero for [`Inside`](Anchor::Inside), which does not climb at all — it looks down.
    #[must_use]
    pub const fn levels(self) -> u8 {
        match self {
            Self::Sibling | Self::Inside => 0,
            Self::Ancestor(n) => n,
        }
    }

    /// Whether the marker is looked for inside the candidate rather than around it.
    #[must_use]
    pub const fn is_inside(self) -> bool {
        matches!(self, Self::Inside)
    }
}

/// One removable path, relative to its ecosystem's anchor directory.
///
/// `path` may be a bare name (`target/`), a glob (`cmake-build-*/`), a multi-segment path
/// (`vendor/bundle/`), or a file pattern (`*.tsbuildinfo`). A trailing `/` means the candidate
/// must be a directory; without one, a file or a directory both qualify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Artifact {
    /// Path to remove, relative to the anchor directory. Never absolute, never contains `..`.
    pub path: &'static str,
    /// Whether this artifact participates without an explicit opt-in.
    pub default_on: bool,
    /// Why the artifact is off by default. Printed by `voom catalog`.
    pub note: Option<&'static str>,
    /// Overrides the ecosystem's anchor for this artifact alone.
    pub anchor: Option<Anchor>,
    /// Overrides the slug derived from `path`, for the rare pair that would derive the same
    /// one (Zig's current `.zig-cache/` and its legacy `zig-cache/`).
    pub slug: Option<&'static str>,
}

impl Artifact {
    /// An artifact that participates by default.
    #[must_use]
    pub const fn on(path: &'static str) -> Self {
        Self {
            path,
            default_on: true,
            note: None,
            anchor: None,
            slug: None,
        }
    }

    /// An artifact that requires an explicit opt-in, with the reason it does.
    ///
    /// Used when the name is also a plausible source directory within the same ecosystem
    /// (`build/` under a `package.json`) or when removal is expensive rather than cheap
    /// (`.venv/`, which costs a reinstall).
    #[must_use]
    pub const fn off(path: &'static str, note: &'static str) -> Self {
        Self {
            path,
            default_on: false,
            note: Some(note),
            anchor: None,
            slug: None,
        }
    }

    /// Overrides the ecosystem's anchor for this artifact.
    #[must_use]
    pub const fn at(mut self, anchor: Anchor) -> Self {
        self.anchor = Some(anchor);
        self
    }

    /// Overrides the derived slug. Needed only when two artifacts in one ecosystem would
    /// otherwise derive the same one.
    #[must_use]
    pub const fn named(mut self, slug: &'static str) -> Self {
        self.slug = Some(slug);
        self
    }

    /// The anchor this artifact resolves against, falling back to its ecosystem's.
    #[must_use]
    pub const fn anchor_in(&self, ecosystem: &Ecosystem) -> Anchor {
        match self.anchor {
            Some(anchor) => anchor,
            None => ecosystem.anchor,
        }
    }

    /// Whether the candidate must be a directory. A trailing `/` in `path` says so.
    #[must_use]
    pub const fn requires_dir(&self) -> bool {
        self.path.as_bytes()[self.path.len() - 1] == b'/'
    }

    /// The artifact path split into segments, with any trailing `/` removed.
    pub fn segments(&self) -> impl Iterator<Item = &'static str> {
        self.path.trim_end_matches('/').split('/')
    }

    /// The short name this artifact answers to in configuration (`python.venv`, `node.build`)
    /// and in `voom catalog`.
    ///
    /// Derived rather than declared so it cannot drift from `path`. Uniqueness within an
    /// ecosystem is asserted by `artifact_slugs_are_unique_within_an_ecosystem`.
    #[must_use]
    pub fn slug(&self) -> String {
        if let Some(slug) = self.slug {
            return slug.to_owned();
        }
        let flattened: String = self
            .path
            .trim_end_matches('/')
            .chars()
            .filter(|character| *character != '*')
            .map(|character| if character == '/' { '-' } else { character })
            .collect();
        let trimmed = flattened.replace("-.", "-").trim_matches(['.', '-']).to_owned();
        if trimmed.is_empty() { flattened } else { trimmed }
    }

    /// How many path segments deep the artifact sits below its anchor directory.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.segments().count()
    }
}

/// One ecosystem: what proves it, and what it leaves behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ecosystem {
    /// Stable slug used in configuration and reports. Unique across the catalog.
    pub id: &'static str,
    /// Human label for reporting.
    pub name: &'static str,
    /// Globs that prove the ecosystem. **At least one is mandatory** — an entry without a
    /// marker would be name-only matching, which `adrs/0002-marker-anchored-classification.md`
    /// forbids and `every_entry_has_at_least_one_marker` rejects.
    pub markers: &'static [&'static str],
    /// Default anchor for this ecosystem's artifacts.
    pub anchor: Anchor,
    /// Paths to remove, relative to the anchor directory.
    pub artifacts: &'static [Artifact],
}

impl Ecosystem {
    /// The artifact with this path, if the ecosystem declares one.
    #[must_use]
    pub fn artifact(&self, path: &str) -> Option<&'static Artifact> {
        self.artifacts.iter().find(|artifact| artifact.path == path)
    }
}

/// The complete built-in catalog.
///
/// Order is stable and is the order `voom catalog` prints. The index of an entry is used as
/// its bit position in the classifier's marker bitmask, so entries may be appended and
/// reordered freely but the table may not exceed [`MAX_ECOSYSTEMS`].
pub static CATALOG: &[Ecosystem] = &[
    native::RUST,
    web::NODE,
    python::PYTHON,
    native::GO,
    native::ZIG,
    apple::SWIFT,
    apple::XCODE,
    managed::MAVEN,
    managed::GRADLE,
    managed::DOTNET,
    native::CMAKE,
    scripting::DART,
    functional::ELIXIR,
    scripting::RUBY,
    web::PHP,
    managed::SCALA,
    functional::HASKELL,
    functional::OCAML,
    scientific::JULIA,
    scientific::R,
    native::NIM,
    web::ELM,
    infra::TERRAFORM,
    infra::BAZEL,
    tooling::ALEF,
    tooling::BASEMIND,
];

/// The classifier packs "which ecosystems have a marker here" into a `u32` bitmask, one bit
/// per catalog index, so a single `read_dir` answers the marker question for every ecosystem
/// at once. Growing the catalog past this needs a wider mask, not a bigger number here.
pub const MAX_ECOSYSTEMS: usize = u32::BITS as usize;

/// Directory names that are dependency caches, not build output.
///
/// They are network-fetched, so removing one costs a re-download and can break offline work.
/// The scanner never *descends* into one — that pruning is where most of the walk's saving
/// comes from — and the catalog never turns one on by default.
///
/// Since the amendment to `adrs/0001-mission-and-scope.md` they are no longer absent from the
/// catalog: several are declared as ordinary off-by-default artifacts so that a user who wants
/// them gone can opt in with `--clean-dependencies` (or the individual specs) instead of
/// hand-writing an unanchored `include`. A marker still has to prove each one, and a default
/// run still leaves every one of them alone —
/// `a_dependency_directory_artifact_is_never_on_by_default` is what keeps that true.
pub const DEPENDENCY_DIRS: &[&str] = &["node_modules", "vendor", "deps", "bower_components"];

/// The artifact specs `--clean-dependencies` turns on.
///
/// The flag is pure sugar: it is exactly equivalent to naming each of these to `--enable`, so
/// it routes through the same [`Selection`](crate::classify::Selection) path as any other
/// opt-in and inherits configuration layering, keep policies and reporting unchanged.
///
/// `python.venv` is here because a virtualenv is the same kind of thing — a fetched tree whose
/// removal costs a reinstall rather than a rebuild — even though `.venv` is not one of the
/// directory names in [`DEPENDENCY_DIRS`].
pub const DEPENDENCY_ARTIFACTS: &[&str] = &[
    "node.node_modules",
    "rust.vendor",
    "php.vendor",
    "go.vendor",
    "elixir.deps",
    "python.venv",
];

/// Whether an `<ecosystem>.<artifact>` spec is one of the dependency directories
/// `--clean-dependencies` reaches.
///
/// The report uses this to name the flag in its footer: removing a dependency directory is the
/// one thing voom's documentation promised would never happen, so a run that did it has to say
/// so rather than let the removal pass as ordinary build output.
#[must_use]
pub fn is_dependency_artifact(spec: &str) -> bool {
    DEPENDENCY_ARTIFACTS.contains(&spec)
}

/// The ecosystem with this id, if the catalog declares one.
#[must_use]
pub fn find(id: &str) -> Option<&'static Ecosystem> {
    CATALOG.iter().find(|ecosystem| ecosystem.id == id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// ADR 0002's central rule, asserted over the whole table. An entry with no marker is
    /// name-only matching, which is how `find -name target -exec rm -rf` loses people's work.
    #[test]
    fn every_entry_has_at_least_one_marker() {
        for ecosystem in CATALOG {
            assert!(
                !ecosystem.markers.is_empty(),
                "ecosystem `{}` has no marker glob",
                ecosystem.id
            );
            for marker in ecosystem.markers {
                assert!(
                    !marker.is_empty(),
                    "ecosystem `{}` has an empty marker glob",
                    ecosystem.id
                );
            }
        }
    }

    #[test]
    fn every_id_is_unique() {
        let mut seen = HashSet::new();
        for ecosystem in CATALOG {
            assert!(seen.insert(ecosystem.id), "duplicate ecosystem id `{}`", ecosystem.id);
        }
    }

    #[test]
    fn every_entry_declares_at_least_one_artifact() {
        for ecosystem in CATALOG {
            assert!(
                !ecosystem.artifacts.is_empty(),
                "ecosystem `{}` removes nothing",
                ecosystem.id
            );
        }
    }

    /// An artifact path is resolved against an anchor directory, so it must stay under it.
    /// A `..` or an absolute component would let a catalog entry address a path the walker
    /// never produced, which `adrs/0006-deletion-semantics.md` forbids.
    #[test]
    fn no_artifact_path_escapes_its_anchor() {
        for ecosystem in CATALOG {
            for artifact in ecosystem.artifacts {
                let path = artifact.path;
                assert!(
                    !path.is_empty(),
                    "ecosystem `{}` has an empty artifact path",
                    ecosystem.id
                );
                assert!(
                    !path.starts_with('/'),
                    "`{}`: artifact `{path}` is absolute",
                    ecosystem.id
                );
                assert!(
                    !path.starts_with('~'),
                    "`{}`: artifact `{path}` is home-relative",
                    ecosystem.id
                );
                for segment in artifact.segments() {
                    assert!(
                        !segment.is_empty(),
                        "`{}`: artifact `{path}` has an empty segment",
                        ecosystem.id
                    );
                    assert_ne!(
                        segment, "..",
                        "`{}`: artifact `{path}` escapes its anchor",
                        ecosystem.id
                    );
                    assert_ne!(segment, ".", "`{}`: artifact `{path}` has a `.` segment", ecosystem.id);
                }
            }
        }
    }

    /// A dependency directory may be in the catalog, and may never be swept without being asked
    /// for by name.
    ///
    /// This used to forbid the entry outright. ADR 0001's amendment allows it, and the property
    /// that mattered is unchanged and still the thing asserted here: `node_modules/` and the
    /// rest cannot be removed by a run that did not explicitly opt in, because `default_on` is
    /// what a default run reads and it is false for every one of them. The mandatory note is
    /// the second half — `voom catalog` and the `--verbose` skip line both print it, so a user
    /// meeting the opt-in is told what it costs before they take it.
    ///
    /// `vendor/bundle/` is not one of these: it names Ruby's build output *inside* `vendor/`,
    /// not `vendor/` itself, and stays on by default.
    #[test]
    fn a_dependency_directory_artifact_is_never_on_by_default() {
        for ecosystem in CATALOG {
            for artifact in ecosystem.artifacts {
                let whole = artifact.path.trim_end_matches('/');
                if !DEPENDENCY_DIRS.contains(&whole) {
                    continue;
                }
                assert!(
                    !artifact.default_on,
                    "`{}`: artifact `{}` is a dependency directory and is on by default — a run \
                     that asked for nothing would delete a network-fetched cache",
                    ecosystem.id, artifact.path
                );
                assert!(
                    artifact.note.is_some(),
                    "`{}`: dependency directory `{}` has no note saying what removing it costs",
                    ecosystem.id,
                    artifact.path
                );
            }
        }
    }

    /// `--clean-dependencies` is sugar over these specs, so each has to name a real declaration
    /// and each has to be an opt-in — otherwise the flag would either fail at parse time or be
    /// enabling something a default run already took.
    #[test]
    fn every_dependency_artifact_spec_names_an_opt_in_declaration() {
        for spec in DEPENDENCY_ARTIFACTS {
            let id = crate::classify::ArtifactId::from_spec(spec)
                .unwrap_or_else(|| panic!("`{spec}` is in DEPENDENCY_ARTIFACTS but not in the catalog"));
            assert!(
                !id.artifact().default_on,
                "`{spec}` is enabled by --clean-dependencies but is already on by default"
            );
            assert!(is_dependency_artifact(spec), "`{spec}` must answer to the predicate");
        }
        assert!(
            !is_dependency_artifact("rust.target"),
            "an ordinary artifact is not a dependency directory"
        );
    }

    /// Every off-by-default artifact explains itself, because `voom catalog` prints the note
    /// and a bare opt-in with no reason is not reviewable.
    #[test]
    fn every_opt_in_artifact_has_a_note() {
        for ecosystem in CATALOG {
            for artifact in ecosystem.artifacts {
                if !artifact.default_on {
                    assert!(
                        artifact.note.is_some(),
                        "`{}`: artifact `{}` is off by default with no note",
                        ecosystem.id,
                        artifact.path
                    );
                }
            }
        }
    }

    #[test]
    fn catalog_fits_the_marker_bitmask() {
        assert!(
            CATALOG.len() <= MAX_ECOSYSTEMS,
            "catalog has {} entries but the marker bitmask holds {MAX_ECOSYSTEMS}",
            CATALOG.len()
        );
    }

    #[test]
    fn ancestor_anchors_are_bounded() {
        for ecosystem in CATALOG {
            for artifact in ecosystem.artifacts {
                let levels = artifact.anchor_in(ecosystem).levels();
                assert!(
                    levels <= 8,
                    "`{}`: artifact `{}` climbs {levels} levels",
                    ecosystem.id,
                    artifact.path
                );
            }
        }
    }

    /// The classifier keys candidates on an artifact's *last* segment and then verifies the
    /// segments above it with a byte comparison. That is only sound while the leading segments
    /// are literals, so this is the invariant that keeps the hot path allocation-free.
    #[test]
    fn leading_artifact_segments_are_literal() {
        for ecosystem in CATALOG {
            for artifact in ecosystem.artifacts {
                let segments: Vec<_> = artifact.segments().collect();
                for segment in &segments[..segments.len() - 1] {
                    assert!(
                        crate::names::is_literal(segment),
                        "`{}`: artifact `{}` has the non-literal leading segment `{segment}`",
                        ecosystem.id,
                        artifact.path
                    );
                }
            }
        }
    }

    /// A marker cannot live inside a file, so an `Inside` artifact has to be a directory. An
    /// entry that forgot the trailing slash would silently never prove anything.
    #[test]
    fn every_inside_anchored_artifact_requires_a_directory() {
        for ecosystem in CATALOG {
            for artifact in ecosystem.artifacts {
                if artifact.anchor_in(ecosystem).is_inside() {
                    assert!(
                        artifact.requires_dir(),
                        "`{}`: artifact `{}` anchors Inside but does not require a directory — \
                         it needs a trailing `/`",
                        ecosystem.id,
                        artifact.path
                    );
                }
            }
        }
    }

    #[test]
    fn artifact_slugs_are_unique_within_an_ecosystem() {
        for ecosystem in CATALOG {
            let mut seen = HashSet::new();
            for artifact in ecosystem.artifacts {
                let slug = artifact.slug();
                assert!(
                    seen.insert(slug.clone()),
                    "`{}`: duplicate artifact slug `{slug}`",
                    ecosystem.id
                );
                assert!(
                    !slug.is_empty(),
                    "`{}`: artifact `{}` has an empty slug",
                    ecosystem.id,
                    artifact.path
                );
            }
        }
    }

    #[test]
    fn slug_strips_leading_dots_globs_and_separators() {
        assert_eq!(Artifact::on(".venv/").slug(), "venv");
        assert_eq!(Artifact::on("build/").slug(), "build");
        assert_eq!(Artifact::on("*.egg-info/").slug(), "egg-info");
        assert_eq!(Artifact::on("cmake-build-*/").slug(), "cmake-build");
        assert_eq!(Artifact::on("deps/build/").slug(), "deps-build");
        assert_eq!(Artifact::on("src/*.o").slug(), "src-o");
    }

    #[test]
    fn requires_dir_follows_the_trailing_slash() {
        assert!(Artifact::on("target/").requires_dir());
        assert!(!Artifact::on("*.tsbuildinfo").requires_dir());
    }

    #[test]
    fn segments_splits_a_multi_segment_artifact() {
        let artifact = Artifact::on("vendor/bundle/");
        assert_eq!(artifact.segments().collect::<Vec<_>>(), vec!["vendor", "bundle"]);
        assert_eq!(artifact.depth(), 2);
    }

    #[test]
    fn find_resolves_a_known_id_and_rejects_an_unknown_one() {
        assert_eq!(find("rust").map(|ecosystem| ecosystem.name), Some("Rust"));
        assert_eq!(find("node_modules"), None);
    }
}
