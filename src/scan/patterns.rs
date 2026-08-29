//! Compiled path globs shared by `exclude` and `include`.
//!
//! Both configured lists (ADR 0004) are evaluated *during* the walk rather than against its
//! results, which is what keeps them free below a match: an excluded directory is never
//! descended into, and an included one is taken whole.

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::error::{Error, Result};

/// A compiled set of configured path globs, and the pattern each match came from.
///
/// Used for both `exclude` and `include` (ADR 0004). Both are evaluated during the walk rather
/// than against its results: an excluded directory is never descended into, and an included one
/// is taken whole, so neither costs anything below itself.
#[derive(Debug, Default)]
pub struct PatternSet {
    patterns: Vec<String>,
    globs: Option<GlobSet>,
}

impl PatternSet {
    /// Compiles the globs once, at configuration load, never per candidate.
    ///
    /// # Errors
    ///
    /// [`Error::CatalogPattern`] if a pattern will not compile.
    pub fn new(patterns: impl IntoIterator<Item = String>) -> Result<Self> {
        let patterns: Vec<String> = patterns.into_iter().collect();
        if patterns.is_empty() {
            return Ok(Self { patterns, globs: None });
        }
        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            let glob = Glob::new(pattern).map_err(|source| Error::CatalogPattern {
                pattern: pattern.clone(),
                source,
            })?;
            builder.add(glob);
        }
        let globs = builder.build().map_err(|source| Error::CatalogPattern {
            pattern: "<exclude set>".to_owned(),
            source,
        })?;
        Ok(Self {
            patterns,
            globs: Some(globs),
        })
    }

    /// The pattern that excludes this path, if any.
    #[must_use]
    pub fn matched(&self, path: &Path) -> Option<&str> {
        let globs = self.globs.as_ref()?;
        globs
            .matches(path)
            .first()
            .and_then(|index| self.patterns.get(*index))
            .map(String::as_str)
    }

    /// Whether anything is excluded at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.globs.is_none()
    }
}
