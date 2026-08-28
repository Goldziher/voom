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
    sources: Vec<PathBuf>,
}

/// The configuration in effect at one point in the tree, with flags applied.
#[derive(Debug, Clone)]
pub struct Resolved {
    selection: Selection,
    keep: KeepPolicy,
    keep_by_ecosystem: BTreeMap<String, KeepPolicy>,
    rules: Vec<Rule>,
    /// Paths never scanned, absolute where the source made them so.
    pub exclude: Vec<String>,
    /// Paths swept without a marker — explicit user intent (ADR 0002).
    pub include: Vec<String>,
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
    /// by every matching `[[paths]]` rule in order.
    #[must_use]
    pub fn keep_for(&self, path: &Path, ecosystem: &str) -> KeepPolicy {
        let mut keep = self.keep;
        if let Some(per_ecosystem) = self.keep_by_ecosystem.get(ecosystem) {
            keep = keep.narrow(*per_ecosystem);
        }
        for rule in &self.rules {
            if rule.glob.is_match(path) {
                keep = keep.narrow(rule.keep);
            }
        }
        keep
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
            keep_by_ecosystem: accumulated.keep_by_ecosystem.clone(),
            rules: accumulated.rules.clone(),
            exclude,
            include: accumulated.include.clone(),
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
    if let Some(rest) = pattern.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).display().to_string();
    }
    if Path::new(pattern).is_absolute() || pattern.starts_with("**") {
        return pattern.to_owned();
    }
    dir.join(pattern).display().to_string()
}
