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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// The marker sits in the anchor directory itself.
    Sibling,
    /// The marker sits in the anchor directory or up to `n` levels above it.
    Ancestor(u8),
}

impl Anchor {
    /// How many levels above the anchor directory the marker search may climb.
    #[must_use]
    pub const fn levels(self) -> u8 {
        match self {
            Self::Sibling => 0,
            Self::Ancestor(n) => n,
        }
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
    /// forbids and `markers_are_mandatory` rejects.
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
];

/// The classifier packs "which ecosystems have a marker here" into a `u32` bitmask, one bit
/// per catalog index, so a single `read_dir` answers the marker question for every ecosystem
/// at once. Growing the catalog past this needs a wider mask, not a bigger number here.
pub const MAX_ECOSYSTEMS: usize = u32::BITS as usize;

/// Directory names that are dependency caches, not build output.
///
/// `adrs/0001-mission-and-scope.md` puts these out of scope entirely: they are network-fetched
/// and removing them costs a re-download and breaks offline work. They are absent from the
/// catalog by construction, never descended into by the scanner, and no flag turns them on.
/// A user who wants them gone names the path explicitly in their own configuration.
pub const DEPENDENCY_DIRS: &[&str] = &["node_modules", "vendor", "deps", "bower_components"];

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

    /// ADR 0001 keeps dependency directories out of the catalog. `vendor/bundle/` is allowed
    /// because it names Ruby's build output *inside* `vendor/`, not `vendor/` itself.
    #[test]
    fn no_artifact_is_a_dependency_directory() {
        for ecosystem in CATALOG {
            for artifact in ecosystem.artifacts {
                let whole = artifact.path.trim_end_matches('/');
                assert!(
                    !DEPENDENCY_DIRS.contains(&whole),
                    "`{}`: artifact `{}` is a dependency directory, which ADR 0001 puts out of scope",
                    ecosystem.id,
                    artifact.path
                );
            }
        }
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
