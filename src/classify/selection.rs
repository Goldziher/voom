//! Which artifacts a run is allowed to consider.
//!
//! [`ArtifactId`] names one declaration in [`CATALOG`] — an ecosystem index and an artifact
//! index within it — so the classifier can carry a two-byte handle through the hot path instead
//! of a string. [`Selection`] is the answer to "may this declaration participate in this run?",
//! resolved once from the merged configuration and then consulted per candidate.

use std::collections::HashMap;

use crate::catalog::{Artifact, CATALOG, Ecosystem};

/// Identifies one artifact declaration: an index into [`CATALOG`] and into that entry's
/// `artifacts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactId {
    pub(super) ecosystem: u8,
    pub(super) artifact: u8,
}

impl ArtifactId {
    /// The catalog entry this artifact belongs to.
    ///
    /// # Panics
    ///
    /// Never in practice: ids are only minted by [`Classifier::new`] from the catalog itself.
    #[must_use]
    pub fn ecosystem(self) -> &'static Ecosystem {
        &CATALOG[self.ecosystem as usize]
    }

    /// The artifact declaration.
    #[must_use]
    pub fn artifact(self) -> &'static Artifact {
        &self.ecosystem().artifacts[self.artifact as usize]
    }

    /// The `<ecosystem>.<artifact>` spelling used in configuration and in `voom catalog`.
    #[must_use]
    pub fn spec(self) -> String {
        format!("{}.{}", self.ecosystem().id, self.artifact().slug())
    }

    /// Resolves an `<ecosystem>.<artifact>` spec back to a declaration.
    ///
    /// Returns `None` for anything the catalog does not answer to, so a typo in configuration
    /// surfaces as a rejected key rather than a rule that silently matches nothing.
    #[must_use]
    pub fn from_spec(spec: &str) -> Option<Self> {
        let (ecosystem_id, artifact_slug) = spec.split_once('.')?;
        let index = CATALOG.iter().position(|ecosystem| ecosystem.id == ecosystem_id)?;
        let offset = CATALOG[index]
            .artifacts
            .iter()
            .position(|artifact| artifact.slug() == artifact_slug)?;
        Some(Self {
            ecosystem: u8::try_from(index).ok()?,
            artifact: u8::try_from(offset).ok()?,
        })
    }
}

/// Which catalog entries participate in a run.
///
/// Starts from the catalog's own `default_on` flags and is narrowed or widened by
/// configuration and flags. Kept separate from the catalog so the table stays pure data.
#[derive(Debug, Clone)]
pub struct Selection {
    ecosystems: u32,
    overrides: HashMap<ArtifactId, bool>,
    permissive: bool,
}

impl Default for Selection {
    fn default() -> Self {
        Self::all()
    }
}

impl Selection {
    /// Every ecosystem participating, each artifact at its catalog default.
    #[must_use]
    pub fn all() -> Self {
        let ecosystems = if CATALOG.len() >= u32::BITS as usize {
            u32::MAX
        } else {
            (1 << CATALOG.len()) - 1
        };
        Self {
            ecosystems,
            overrides: HashMap::new(),
            permissive: false,
        }
    }

    /// Only the named ecosystems participate. Unknown ids are returned rather than ignored,
    /// because a typo in `--ecosystem` should fail loudly rather than silently sweep nothing.
    #[must_use]
    pub fn only<'a>(ids: impl IntoIterator<Item = &'a str>) -> (Self, Vec<String>) {
        let mut selection = Self {
            ecosystems: 0,
            overrides: HashMap::new(),
            permissive: false,
        };
        let mut unknown = Vec::new();
        for id in ids {
            match CATALOG.iter().position(|ecosystem| ecosystem.id == id) {
                Some(index) => selection.ecosystems |= 1 << index,
                None => unknown.push(id.to_owned()),
            }
        }
        (selection, unknown)
    }

    /// Treats every artifact as on unless something turns it off.
    ///
    /// Classification runs permissively so that a `voom.toml` deeper in the tree can still
    /// enable an off-by-default artifact — the final decision is made per artifact against the
    /// configuration resolved at its own location (ADR 0004). It never widens what the command
    /// line allowed, because flags are applied above every configuration layer.
    #[must_use]
    pub fn permissive(mut self) -> Self {
        self.permissive = true;
        self
    }

    /// Overrides one artifact's participation, by `<ecosystem>.<artifact>` spec or by bare
    /// ecosystem id (which sets every artifact of that ecosystem). Returns `false` if nothing
    /// in the catalog answers to the spec.
    ///
    /// **A bare ecosystem id never widens to a dependency directory.** `--enable node` means
    /// "turn on node's off-by-default build output", and before ADR 0001's amendment there was
    /// nothing else it could mean. Letting it now also reach `node_modules/` would make a
    /// `voom.toml` written against 0.1.0 — `[ecosystems] enable = ["go"]`, to get `go.bin` —
    /// start deleting a checked-in `vendor/` on upgrade, silently and in the delete direction.
    /// Those five artifacts are reachable only by their own `<ecosystem>.<artifact>` spec, or
    /// by `--clean-dependencies`, which expands to exactly those specs.
    ///
    /// The restriction is on widening only. A bare id still turns an ecosystem *off* whole,
    /// because narrowing is always safe.
    pub fn set(&mut self, spec: &str, enabled: bool) -> bool {
        let (ecosystem_id, artifact_slug) = match spec.split_once('.') {
            Some((ecosystem_id, artifact_slug)) => (ecosystem_id, Some(artifact_slug)),
            None => (spec, None),
        };
        let Some(index) = CATALOG.iter().position(|ecosystem| ecosystem.id == ecosystem_id) else {
            return false;
        };
        let ecosystem = &CATALOG[index];
        let mut matched = false;
        for (offset, artifact) in ecosystem.artifacts.iter().enumerate() {
            if artifact_slug.is_some_and(|slug| artifact.slug() != slug) {
                continue;
            }
            if artifact_slug.is_none()
                && enabled
                && crate::catalog::is_dependency_artifact(&format!("{ecosystem_id}.{}", artifact.slug()))
            {
                continue;
            }
            let Ok(ecosystem_index) = u8::try_from(index) else {
                continue;
            };
            let Ok(artifact_index) = u8::try_from(offset) else {
                continue;
            };
            self.overrides.insert(
                ArtifactId {
                    ecosystem: ecosystem_index,
                    artifact: artifact_index,
                },
                enabled,
            );
            matched = true;
        }
        if matched && enabled {
            self.ecosystems |= 1 << index;
        }
        matched
    }

    /// Whether this artifact's ecosystem participates at all.
    ///
    /// Checked before marker resolution so a deselected ecosystem never costs a `read_dir`.
    #[must_use]
    pub fn ecosystem_enabled(&self, id: ArtifactId) -> bool {
        self.ecosystems & (1 << id.ecosystem) != 0
    }

    /// Whether this artifact participates.
    #[must_use]
    pub fn is_enabled(&self, id: ArtifactId) -> bool {
        if !self.ecosystem_enabled(id) {
            return false;
        }
        self.overrides
            .get(&id)
            .copied()
            .unwrap_or_else(|| self.permissive || id.artifact().default_on)
    }
}
