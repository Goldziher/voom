//! Merging the layers, and resolving what applies at one point in the tree.
//!
//! Two rules carry the semantics:
//!
//! - **Keep policies compose by narrowing, not replacing.** An override sets only the keys it
//!   names; the rest inherit. Replacement would mean a user who sets one key silently loses
//!   every other guard, which for a safety setting is the wrong failure.
//! - **Flags always win.** Configuration layers accumulate as an ordered list of operations,
//!   and the command line is applied after all of them at every point in the tree — so a
//!   `voom.toml` deep in the tree cannot override what the invocation asked for.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use globset::{Glob, GlobMatcher};

use super::load::{Layer, Sources, read_in};
use super::schema::{EcosystemsSection, KeepTable};
use crate::catalog;
use crate::classify::{ArtifactId, Selection};
use crate::error::{Error, Result};
use crate::policy::{KeepPolicy, parse_duration, parse_size};

/// Command-line overrides, which sit above every configuration layer.
#[derive(Debug, Default, Clone)]
pub struct Flags {
    /// `--ecosystem`: when set, only these participate at all.
    pub ecosystems: Option<Vec<String>>,
    /// `--enable` specs.
    pub enable: Vec<String>,
    /// `--disable` specs.
    pub disable: Vec<String>,
    /// `--min-age`, `--min-size`, `--max-size`.
    pub keep: KeepPolicy,
    /// `--exclude` globs.
    pub exclude: Vec<String>,
    /// `--no-git`, as `Some(false)`. `None` leaves the decision to configuration.
    pub git: Option<bool>,
}

/// One compiled `[[paths]]` rule.
#[derive(Debug, Clone)]
struct Rule {
    pattern: String,
    glob: GlobMatcher,
    keep: KeepPolicy,
    ops: Vec<(String, bool)>,
}

/// The configuration layers accumulated down to one directory, before flags.
#[derive(Debug, Default, Clone)]
struct Accumulated {
    /// Enable/disable operations in precedence order; later wins.
    ops: Vec<(String, bool)>,
    keep: KeepPolicy,
    keep_by_ecosystem: BTreeMap<String, KeepPolicy>,
    rules: Vec<Rule>,
    exclude: Vec<String>,
    include: Vec<String>,
    git: Option<bool>,
    sources: Vec<PathBuf>,
}

/// The configuration in effect at one point in the tree, with flags applied.
#[derive(Debug, Clone)]
pub struct Resolved {
    selection: Selection,
    keep: KeepPolicy,
    /// The keep policy the command line asked for, kept apart from the merged configuration so
    /// that [`Resolved::keep_for`] can apply it last. Folding it into `keep` alone is not
    /// enough: the per-ecosystem and `[[paths]]` overrides narrow that value afterwards, and
    /// narrowing lets the later value win — so a committed `voom.toml` could release an
    /// artifact that `--min-age` was holding.
    flags_keep: KeepPolicy,
    keep_by_ecosystem: BTreeMap<String, KeepPolicy>,
    rules: Vec<Rule>,
    /// Paths never scanned, absolute where the source made them so.
    pub exclude: Vec<String>,
    /// Paths swept without a marker — explicit user intent (ADR 0002).
    pub include: Vec<String>,
    /// Whether an ordinary sweep runs git housekeeping here (ADR 0011). On unless something
    /// turned it off.
    pub git: bool,
    /// The files that contributed, in ascending precedence.
    pub sources: Vec<PathBuf>,
}

impl Resolved {
    /// Whether an artifact participates at this path.
    #[must_use]
    pub fn is_enabled(&self, path: &Path, id: ArtifactId) -> bool {
        let mut selection = None;
        for rule in &self.rules {
            if rule.ops.is_empty() || !rule.glob.is_match(path) {
                continue;
            }
            let selection = selection.get_or_insert_with(|| self.selection.clone());
            for (spec, enabled) in &rule.ops {
                selection.set(spec, *enabled);
            }
        }
        selection.as_ref().unwrap_or(&self.selection).is_enabled(id)
    }

    /// The keep policy for one artifact: the base, narrowed by its ecosystem's override, then
    /// by every matching `[[paths]]` rule in order — and finally by the command line, which
    /// outranks all of them.
    ///
    /// The flags go last because [`KeepPolicy::narrow`] lets the *later* value win, so applying
    /// them to the base alone would leave any config layer named after it free to overrule
    /// them. A repository that ships `[keep.rust] min_age = "0s"` could then release an
    /// artifact that the operator's `--min-age 30d` was holding, which inverts the precedence
    /// this module promises and does it in the unsafe direction.
    #[must_use]
    pub fn keep_for(&self, path: &Path, ecosystem: Option<&str>) -> KeepPolicy {
        let mut keep = self.keep;
        if let Some(per_ecosystem) = ecosystem.and_then(|id| self.keep_by_ecosystem.get(id)) {
            keep = keep.narrow(*per_ecosystem);
        }
        for rule in &self.rules {
            if rule.glob.is_match(path) {
                keep = keep.narrow(rule.keep);
            }
        }
        keep.narrow(self.flags_keep)
    }

    /// The base keep policy, for `voom config show`.
    #[must_use]
    pub fn keep(&self) -> KeepPolicy {
        self.keep
    }

    /// The base selection, for `voom config show`.
    #[must_use]
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The per-ecosystem keep overrides, for `voom config show`.
    pub fn keep_overrides(&self) -> impl Iterator<Item = (&str, KeepPolicy)> {
        self.keep_by_ecosystem.iter().map(|(id, keep)| (id.as_str(), *keep))
    }

    /// The `[[paths]]` rules in precedence order, for `voom config show`.
    pub fn rules(&self) -> impl Iterator<Item = (&str, KeepPolicy, &[(String, bool)])> {
        self.rules
            .iter()
            .map(|rule| (rule.pattern.as_str(), rule.keep, rule.ops.as_slice()))
    }
}

/// Resolves configuration for any directory under a scan root, memoized.
#[derive(Debug)]
pub struct Resolver {
    root: PathBuf,
    flags: Flags,
    base: Accumulated,
    /// Set when `--config` named a file. The hierarchy is then off entirely — one file and no
    /// surprises is what an explicit flag should mean.
    explicit: bool,
    cache: RwLock<HashMap<PathBuf, Arc<Cached>>>,
}

#[derive(Debug)]
struct Cached {
    accumulated: Accumulated,
    effective: Arc<Resolved>,
}

impl Resolver {
    /// Builds the base layers for a root and validates everything they contain.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for an unparseable duration or size, an ecosystem id the catalog does
    /// not know, or an uncompilable glob — each naming the file it came from.
    pub fn new(root: &Path, sources: &Sources, flags: Flags) -> Result<Self> {
        let mut base = Accumulated::default();
        for layer in &sources.layers {
            apply(&mut base, layer)?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            flags,
            base,
            explicit: sources.explicit,
            cache: RwLock::new(HashMap::new()),
        })
    }

    /// The configuration in effect at the scan root.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::for_dir`] rejects.
    pub fn root_config(&self) -> Result<Arc<Resolved>> {
        let root = self.root.clone();
        self.for_dir(&root)
    }

    /// The configuration in effect in a directory: every `voom.toml` from the scan root down to
    /// it, nearest wins, with flags on top.
    ///
    /// Memoized per directory and derived from the parent's result, so a wide tree resolves in
    /// one pass rather than re-reading the same ancestors for every sibling.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] if a `voom.toml` on the way down is broken.
    pub fn for_dir(&self, dir: &Path) -> Result<Arc<Resolved>> {
        Ok(self.cached(dir)?.effective.clone())
    }

    fn cached(&self, dir: &Path) -> Result<Arc<Cached>> {
        if let Ok(cache) = self.cache.read()
            && let Some(cached) = cache.get(dir)
        {
            return Ok(cached.clone());
        }

        let mut accumulated = if dir == self.root {
            self.base.clone()
        } else {
            match dir.parent() {
                // Stop at the scan root: a `voom.toml` above the tree voom was pointed at is
                // not part of this run.
                Some(parent) if dir.starts_with(&self.root) => self.cached(parent)?.accumulated.clone(),
                _ => self.base.clone(),
            }
        };

        if !self.explicit
            && let Some(layer) = read_in(dir)?
        {
            apply(&mut accumulated, &layer)?;
        }

        let effective = Arc::new(self.effective(&accumulated)?);
        let cached = Arc::new(Cached { accumulated, effective });
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(dir.to_path_buf(), cached.clone());
        }
        Ok(cached)
    }

    /// Applies the command line on top of the accumulated layers.
    fn effective(&self, accumulated: &Accumulated) -> Result<Resolved> {
        let mut selection = match &self.flags.ecosystems {
            Some(ids) => {
                let (selection, unknown) = Selection::only(ids.iter().map(String::as_str));
                if let Some(name) = unknown.first() {
                    return Err(Error::UnknownEcosystem { name: name.clone() });
                }
                selection
            }
            None => Selection::all(),
        };

        for (spec, enabled) in &accumulated.ops {
            selection.set(spec, *enabled);
        }
        // Flags last, so no configuration file can override the invocation.
        for spec in &self.flags.disable {
            if !selection.set(spec, false) {
                return Err(Error::UnknownEcosystem { name: spec.clone() });
            }
        }
        for spec in &self.flags.enable {
            if !selection.set(spec, true) {
                return Err(Error::UnknownEcosystem { name: spec.clone() });
            }
        }

        let mut exclude = accumulated.exclude.clone();
        exclude.extend(self.flags.exclude.iter().cloned());

        Ok(Resolved {
            selection,
            keep: accumulated.keep.narrow(self.flags.keep),
            flags_keep: self.flags.keep,
            keep_by_ecosystem: accumulated.keep_by_ecosystem.clone(),
            rules: accumulated.rules.clone(),
            exclude,
            include: accumulated.include.clone(),
            // Flags last, on the same principle as the selection above: no `voom.toml` below a
            // scan root may turn housekeeping back on for a run that was invoked with
            // `--no-git`. On by default, which is what makes it ordinary behaviour.
            git: self.flags.git.or(accumulated.git).unwrap_or(true),
            sources: accumulated.sources.clone(),
        })
    }
}

/// Folds one file into the accumulated layers, validating as it goes.
fn apply(accumulated: &mut Accumulated, layer: &Layer) -> Result<()> {
    let source = &layer.source;
    let dir = source.parent().unwrap_or(Path::new("."));

    for spec in &layer.file.ecosystems.disable {
        validate_spec(spec, source)?;
        accumulated.ops.push((spec.clone(), false));
    }
    for spec in &layer.file.ecosystems.enable {
        validate_spec(spec, source)?;
        accumulated.ops.push((spec.clone(), true));
    }

    // Later layer wins, matching every other scalar key: a repository's committed `voom.toml`
    // decides for its own subtree.
    if let Some(enabled) = layer.file.git.enabled {
        accumulated.git = Some(enabled);
    }

    accumulated.keep = accumulated.keep.narrow(keep_from(&layer.file.keep.scalars(), source)?);
    for (id, table) in &layer.file.keep.per_ecosystem {
        if catalog::find(id).is_none() {
            return Err(Error::Config {
                path: source.clone(),
                reason: format!("`[keep.{id}]` is not a known ecosystem"),
            });
        }
        let policy = keep_from(table, source)?;
        let entry = accumulated.keep_by_ecosystem.entry(id.clone()).or_default();
        *entry = entry.narrow(policy);
    }

    for pattern in &layer.file.exclude {
        accumulated.exclude.push(expand(pattern, dir));
    }
    for pattern in &layer.file.include {
        accumulated.include.push(expand(pattern, dir));
    }

    for rule in &layer.file.paths {
        let pattern = expand(&rule.pattern, dir);
        let glob = Glob::new(&pattern)
            .map_err(|error| Error::Config {
                path: source.clone(),
                reason: format!("`[[paths]] match = \"{pattern}\"` is not a valid glob: {error}"),
            })?
            .compile_matcher();
        let keep = match &rule.keep {
            Some(table) => keep_from(table, source)?,
            None => KeepPolicy::default(),
        };
        accumulated.rules.push(Rule {
            pattern,
            glob,
            keep,
            ops: rule_ops(rule.ecosystems.as_ref(), source)?,
        });
    }

    accumulated.sources.push(source.clone());
    Ok(())
}

fn rule_ops(section: Option<&EcosystemsSection>, source: &Path) -> Result<Vec<(String, bool)>> {
    let Some(section) = section else { return Ok(Vec::new()) };
    let mut ops = Vec::new();
    for spec in &section.disable {
        validate_spec(spec, source)?;
        ops.push((spec.clone(), false));
    }
    for spec in &section.enable {
        validate_spec(spec, source)?;
        ops.push((spec.clone(), true));
    }
    Ok(ops)
}

/// Rejects a spec the catalog does not answer to, naming the file it came from.
fn validate_spec(spec: &str, source: &Path) -> Result<()> {
    let known = match spec.split_once('.') {
        Some((ecosystem, _)) => ArtifactId::from_spec(spec).is_some() && catalog::find(ecosystem).is_some(),
        None => catalog::find(spec).is_some(),
    };
    if known {
        return Ok(());
    }
    Err(Error::Config {
        path: source.to_path_buf(),
        reason: format!("`{spec}` is not a known ecosystem or artifact — run `voom catalog` to see the table"),
    })
}

/// Parses a keep table strictly, attributing any failure to the file it came from.
fn keep_from(table: &KeepTable, source: &Path) -> Result<KeepPolicy> {
    let attribute = |error: Error| Error::Config {
        path: source.to_path_buf(),
        reason: error.to_string(),
    };
    Ok(KeepPolicy {
        min_age: table
            .min_age
            .as_deref()
            .map(parse_duration)
            .transpose()
            .map_err(attribute)?,
        min_size: table
            .min_size
            .as_deref()
            .map(parse_size)
            .transpose()
            .map_err(attribute)?,
        max_size: table
            .max_size
            .as_deref()
            .map(parse_size)
            .transpose()
            .map_err(attribute)?,
    })
}

/// Expands `~` and makes a relative pattern relative to the file that declared it.
///
/// ADR 0004 says paths are "relative to this file or absolute", which is what makes a committed
/// `voom.toml` portable between checkouts.
fn expand(pattern: &str, dir: &Path) -> String {
    // `~\` as well as `~/`: a Windows user types the separator their shell shows them, and a
    // home prefix that silently became a relative path would point the pattern somewhere else
    // entirely rather than fail.
    if let Some(rest) = pattern.strip_prefix("~/").or_else(|| pattern.strip_prefix("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return as_glob(&home.join(rest));
    }
    if Path::new(pattern).is_absolute() || pattern.starts_with("**") {
        return pattern.to_owned();
    }
    as_glob(&dir.join(pattern))
}

/// Renders a path as a glob pattern, which is always `/`-separated.
///
/// `Path::display` emits the platform's separator, and on Windows that is `\`, which glob syntax
/// reads as an escape rather than a separator — an `exclude` built out of a rendered path is one
/// escape away from quietly matching nothing, and an exclusion that stops working silently is
/// what ADR 0004 exists to prevent. `globset` matches against a candidate it has already
/// normalized to `/`, so `/` is the separator both sides agree on. Only [`MAIN_SEPARATOR`] is
/// rewritten, which leaves a unix filename that genuinely contains a backslash alone.
///
/// [`MAIN_SEPARATOR`]: std::path::MAIN_SEPARATOR
fn as_glob(path: &Path) -> String {
    path.display().to_string().replace(std::path::MAIN_SEPARATOR, "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load::read_in;
    use crate::scan::PatternSet;
    use crate::testing::tree;

    fn home() -> PathBuf {
        dirs::home_dir().expect("a home directory")
    }

    #[test]
    fn should_expand_a_home_pattern_to_a_slash_separated_glob() {
        let expanded = expand("~/work/client-x/**", Path::new("/anywhere"));
        assert!(
            !expanded.contains('\\'),
            "a backslash escapes in glob syntax: {expanded}"
        );
        assert!(expanded.ends_with("/work/client-x/**"), "{expanded}");
        assert_ne!(expanded, "~/work/client-x/**", "the home prefix must be expanded");
    }

    /// A Windows user types the separator their shell shows them, so `~\` has to mean `~/`.
    #[test]
    fn should_expand_a_backslash_home_prefix_the_same_way() {
        let dir = Path::new("/anywhere");
        assert_eq!(expand("~\\scratch/**", dir), expand("~/scratch/**", dir));
    }

    #[test]
    fn should_expand_a_relative_pattern_to_a_slash_separated_glob() {
        let fixture = tree(&["project/voom.toml"]);
        let expanded = expand("build/**", &fixture.path().join("project"));
        assert!(
            !expanded.contains('\\'),
            "a backslash escapes in glob syntax: {expanded}"
        );
        assert!(expanded.ends_with("/project/build/**"), "{expanded}");
    }

    /// A `**`-prefixed pattern is already root-relative, so it is passed through untouched.
    #[test]
    fn should_pass_a_double_star_pattern_through_unchanged() {
        let expanded = expand("**/fixtures/**", Path::new("/anywhere"));
        assert_eq!(expanded, "**/fixtures/**");
        assert!(!expanded.contains('\\'), "{expanded}");
    }

    #[test]
    fn should_pass_an_absolute_pattern_through_unchanged() {
        let absolute = home().join("work").display().to_string();
        let pattern = format!("{absolute}/**");
        assert_eq!(expand(&pattern, Path::new("/anywhere")), pattern);
    }

    /// The property that silently breaks when the pattern carries platform separators: an
    /// `exclude` written in a `voom.toml` must still match the path it names.
    #[test]
    fn should_produce_an_exclude_that_matches_the_path_it_names() {
        let fixture = tree(&["project/voom.toml", "project/build/output.o"]);
        let dir = fixture.path().join("project");
        std::fs::write(dir.join("voom.toml"), "exclude = [\"build/**\"]\n").unwrap();

        let layer = read_in(&dir).expect("it parses").expect("a layer");
        let mut accumulated = Accumulated::default();
        apply(&mut accumulated, &layer).expect("the layer applies");

        let excludes = PatternSet::new(accumulated.exclude.clone()).expect("the exclude compiles");
        assert_eq!(
            excludes.matched(&dir.join("build").join("output.o")),
            Some(accumulated.exclude[0].as_str())
        );
    }

    /// The same property for a `~`-rooted exclude, where the expansion is a real system path and
    /// therefore carries whatever separator the platform renders.
    #[test]
    fn should_produce_a_home_exclude_that_matches_the_path_it_names() {
        let pattern = expand("~/work/client-x/**", Path::new("/anywhere"));
        let excludes = PatternSet::new([pattern]).expect("the exclude compiles");
        let candidate = home().join("work").join("client-x").join("target");
        assert!(excludes.matched(&candidate).is_some(), "{}", candidate.display());
    }

    /// A `[[paths]]` rule compiles its pattern the same way, and must match the same path.
    #[test]
    fn should_produce_a_path_rule_that_matches_the_path_it_names() {
        let fixture = tree(&["project/voom.toml"]);
        let dir = fixture.path().join("project");
        std::fs::write(
            dir.join("voom.toml"),
            "[[paths]]\nmatch = \"vendor/**\"\n[paths.keep]\nmin_age = \"7d\"\n",
        )
        .unwrap();

        let layer = read_in(&dir).expect("it parses").expect("a layer");
        let mut accumulated = Accumulated::default();
        apply(&mut accumulated, &layer).expect("the layer applies");

        let rule = accumulated.rules.first().expect("one rule");
        assert!(!rule.pattern.contains('\\'), "{}", rule.pattern);
        assert!(rule.glob.is_match(dir.join("vendor").join("target")));
    }
}
