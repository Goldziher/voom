//! Allocation-free file-name pattern matching.
//!
//! Both halves of classification ask the same question millions of times: "does this file name
//! match any of these patterns, and if so, which?" Marker resolution asks it of every name in a
//! probed directory; candidate matching asks it of every entry the walker produces.
//!
//! Almost every pattern in the catalog is a literal (`Cargo.toml`), a suffix (`*.csproj`), or a
//! prefix (`bazel-*`). Those three are answered by a hash lookup or a byte comparison with no
//! allocation and no regex engine. A genuinely complex pattern falls back to a compiled
//! [`GlobSet`], which is built once and never per candidate.
//!
//! Names are matched as [`OsStr`] bytes rather than `&str`: a non-UTF-8 filename is a thing that
//! exists, and a filesystem tool will meet one. Every catalog pattern is ASCII, so comparing
//! encoded bytes is both correct and lossless.

use std::collections::HashMap;
use std::ffi::OsStr;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::error::{Error, Result};

/// Characters that make a pattern more than a literal, prefix, or suffix.
const GLOB_META: [char; 5] = ['*', '?', '[', ']', '{'];

/// Whether a pattern is a plain literal name with no glob syntax at all.
#[must_use]
pub fn is_literal(pattern: &str) -> bool {
    !pattern.contains(GLOB_META)
}

/// How a pattern will be evaluated.
enum Shape {
    /// The whole name, compared byte for byte.
    Literal,
    /// `foo*` — everything after the leading literal is free.
    Prefix(&'static str),
    /// `*.foo` — everything before the trailing literal is free.
    Suffix(&'static str),
    /// Anything else; costs a compiled glob.
    Glob,
}

fn shape_of(pattern: &'static str) -> Shape {
    if is_literal(pattern) {
        return Shape::Literal;
    }
    if let Some(rest) = pattern.strip_prefix('*')
        && is_literal(rest)
    {
        return Shape::Suffix(rest);
    }
    if let Some(rest) = pattern.strip_suffix('*')
        && is_literal(rest)
    {
        return Shape::Prefix(rest);
    }
    Shape::Glob
}

/// A set of file-name patterns, each carrying a value, indexed for cheap lookup.
#[derive(Debug)]
pub struct NameMatcher<T> {
    literals: HashMap<&'static [u8], Vec<T>>,
    prefixes: Vec<(&'static [u8], T)>,
    suffixes: Vec<(&'static [u8], T)>,
    globs: GlobSet,
    glob_values: Vec<T>,
}

impl<T: Copy> NameMatcher<T> {
    /// Indexes `(pattern, value)` pairs. Duplicate patterns are kept, not merged — two
    /// ecosystems may legitimately share a marker name.
    ///
    /// # Errors
    ///
    /// [`Error::CatalogPattern`] if a pattern needs a glob and will not compile.
    pub fn build(entries: impl IntoIterator<Item = (&'static str, T)>) -> Result<Self> {
        let mut literals: HashMap<&'static [u8], Vec<T>> = HashMap::new();
        let mut prefixes = Vec::new();
        let mut suffixes = Vec::new();
        let mut glob_builder = GlobSetBuilder::new();
        let mut glob_values = Vec::new();

        for (pattern, value) in entries {
            match shape_of(pattern) {
                Shape::Literal => literals.entry(pattern.as_bytes()).or_default().push(value),
                Shape::Prefix(literal) => prefixes.push((literal.as_bytes(), value)),
                Shape::Suffix(literal) => suffixes.push((literal.as_bytes(), value)),
                Shape::Glob => {
                    let glob = Glob::new(pattern).map_err(|source| Error::CatalogPattern {
                        pattern: pattern.to_owned(),
                        source,
                    })?;
                    glob_builder.add(glob);
                    glob_values.push(value);
                }
            }
        }

        let globs = glob_builder.build().map_err(|source| Error::CatalogPattern {
            pattern: "<glob set>".to_owned(),
            source,
        })?;

        Ok(Self {
            literals,
            prefixes,
            suffixes,
            globs,
            glob_values,
        })
    }

    /// Calls `visit` once per matching pattern, in no particular order.
    ///
    /// This runs on every directory entry the walker produces, so it allocates nothing on the
    /// literal, prefix, and suffix paths and touches the glob engine only when the catalog
    /// actually contains a pattern needing one.
    pub fn for_each_match(&self, name: &OsStr, mut visit: impl FnMut(T)) {
        let bytes = name.as_encoded_bytes();

        if let Some(values) = self.literals.get(bytes) {
            for value in values {
                visit(*value);
            }
        }
        for (literal, value) in &self.suffixes {
            if bytes.ends_with(literal) {
                visit(*value);
            }
        }
        for (literal, value) in &self.prefixes {
            if bytes.starts_with(literal) {
                visit(*value);
            }
        }
        if !self.glob_values.is_empty()
            && let Some(name) = name.to_str()
        {
            for index in self.globs.matches(name) {
                if let Some(value) = self.glob_values.get(index) {
                    visit(*value);
                }
            }
        }
    }

    /// Whether any pattern needed the glob engine. Used by tests to assert the catalog stays
    /// on the allocation-free path.
    #[must_use]
    pub fn uses_glob_engine(&self) -> bool {
        !self.glob_values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(matcher: &NameMatcher<u8>, name: &str) -> Vec<u8> {
        let mut hits = Vec::new();
        matcher.for_each_match(OsStr::new(name), |value| hits.push(value));
        hits.sort_unstable();
        hits
    }

    #[test]
    fn should_match_a_literal_pattern_exactly() {
        let matcher = NameMatcher::build([("Cargo.toml", 1u8)]).unwrap();
        assert_eq!(collect(&matcher, "Cargo.toml"), vec![1]);
        assert_eq!(collect(&matcher, "Cargo.lock"), Vec::<u8>::new());
        assert_eq!(collect(&matcher, "xCargo.toml"), Vec::<u8>::new());
    }

    #[test]
    fn should_match_a_suffix_pattern_without_the_glob_engine() {
        let matcher = NameMatcher::build([("*.csproj", 7u8)]).unwrap();
        assert!(!matcher.uses_glob_engine());
        assert_eq!(collect(&matcher, "App.csproj"), vec![7]);
        assert_eq!(collect(&matcher, "App.fsproj"), Vec::<u8>::new());
    }

    #[test]
    fn should_match_a_prefix_pattern_without_the_glob_engine() {
        let matcher = NameMatcher::build([("bazel-*", 3u8)]).unwrap();
        assert!(!matcher.uses_glob_engine());
        assert_eq!(collect(&matcher, "bazel-bin"), vec![3]);
        assert_eq!(collect(&matcher, "bazelisk"), Vec::<u8>::new());
    }

    #[test]
    fn should_report_every_pattern_that_matches_a_name() {
        let matcher = NameMatcher::build([("*.tf", 1u8), ("main.tf", 2u8), ("main*", 3u8)]).unwrap();
        assert_eq!(collect(&matcher, "main.tf"), vec![1, 2, 3]);
    }

    #[test]
    fn should_fall_back_to_the_glob_engine_for_a_complex_pattern() {
        let matcher = NameMatcher::build([("cmake-build-*-debug", 9u8)]).unwrap();
        assert!(matcher.uses_glob_engine());
        assert_eq!(collect(&matcher, "cmake-build-arm-debug"), vec![9]);
        assert_eq!(collect(&matcher, "cmake-build-arm"), Vec::<u8>::new());
    }

    #[test]
    fn should_not_match_a_non_utf8_name_against_an_ascii_pattern() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let matcher = NameMatcher::build([("target", 1u8)]).unwrap();
            let name = OsStr::from_bytes(b"targ\xffet");
            let mut hits = Vec::new();
            matcher.for_each_match(name, |value| hits.push(value));
            assert!(hits.is_empty());
        }
    }

    #[test]
    fn should_reject_an_uncompilable_pattern() {
        let error = NameMatcher::build([("[", 1u8)]).unwrap_err();
        assert!(matches!(error, Error::CatalogPattern { .. }));
    }
}
