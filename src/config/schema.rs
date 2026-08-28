//! The on-disk TOML schema.
//!
//! Every struct is `deny_unknown_fields`. A typo in a safety-relevant key like `exclude` must
//! fail loudly rather than quietly disabling the guard the user thought they had (ADR 0004).
//! Values are kept as strings here and parsed in [`super::resolve`], so a bad duration names
//! the file it came from.

use std::collections::BTreeMap;

use serde::Deserialize;

/// One `voom.toml` or `config.toml`, exactly as written.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Which catalog entries participate.
    #[serde(default)]
    pub ecosystems: EcosystemsSection,
    /// Paths never scanned or deleted. Always wins.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Extra paths to sweep that no marker anchors.
    #[serde(default)]
    pub include: Vec<String>,
    /// Keep policies, with optional per-ecosystem overrides.
    #[serde(default)]
    pub keep: KeepSection,
    /// Per-path overrides, glob-matched; later entries win.
    #[serde(default)]
    pub paths: Vec<PathRule>,
    /// Git housekeeping (ADR 0011), which an ordinary sweep runs.
    #[serde(default)]
    pub git: GitSection,
}

/// `[git]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSection {
    /// Whether an ordinary sweep runs git's own local housekeeping in the repositories it walks
    /// past. Unset inherits the layer above; `--no-git` overrides every layer.
    pub enabled: Option<bool>,
}

/// `[ecosystems]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EcosystemsSection {
    /// Turn on an off-by-default artifact, by `<ecosystem>.<artifact>` spec.
    #[serde(default)]
    pub enable: Vec<String>,
    /// Turn off an ecosystem or one of its artifacts.
    #[serde(default)]
    pub disable: Vec<String>,
}

/// `[keep]`, whose sub-tables are ecosystem ids (`[keep.rust]`).
///
/// The flattened map is what allows the mixed shape ADR 0004 specifies. An ecosystem id that is
/// not in the catalog is rejected during resolution, so a typo in `[keep.rustt]` does not
/// silently apply to nothing.
#[derive(Debug, Default, Deserialize)]
pub struct KeepSection {
    /// Never remove an artifact modified more recently than this.
    pub min_age: Option<String>,
    /// Not worth the rebuild below this size.
    pub min_size: Option<String>,
    /// Skip anything larger.
    pub max_size: Option<String>,
    /// Per-ecosystem overrides.
    #[serde(flatten)]
    pub per_ecosystem: BTreeMap<String, KeepTable>,
}

/// A bare keep policy, as it appears in `[keep.rust]` or `[[paths]].keep`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeepTable {
    /// Never remove an artifact modified more recently than this.
    pub min_age: Option<String>,
    /// Not worth the rebuild below this size.
    pub min_size: Option<String>,
    /// Skip anything larger.
    pub max_size: Option<String>,
}

impl KeepSection {
    /// The section's own scalar keys, without the per-ecosystem sub-tables.
    #[must_use]
    pub fn scalars(&self) -> KeepTable {
        KeepTable {
            min_age: self.min_age.clone(),
            min_size: self.min_size.clone(),
            max_size: self.max_size.clone(),
        }
    }
}

/// One `[[paths]]` entry.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRule {
    /// The glob this rule applies to.
    #[serde(rename = "match")]
    pub pattern: String,
    /// Keep policy for matching artifacts.
    #[serde(default)]
    pub keep: Option<KeepTable>,
    /// Ecosystem selection for matching artifacts.
    #[serde(default)]
    pub ecosystems: Option<EcosystemsSection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_parse_the_documented_schema() {
        let config: ConfigFile = toml::from_str(
            r#"
            exclude = ["~/work/client-x/**", "**/fixtures/**"]
            include = ["~/scratch/build-junk"]

            [ecosystems]
            enable  = ["python.venv"]
            disable = ["terraform"]

            [keep]
            min_age  = "7d"
            max_size = "50GB"
            min_size = "1MB"

            [keep.rust]
            min_age = "1d"

            [[paths]]
            match = "~/work/**"
            keep  = { min_age = "30d" }

            [[paths]]
            match      = "~/experiments/**"
            ecosystems = { enable = ["node.build"] }
            "#,
        )
        .expect("the documented example parses");

        assert_eq!(config.ecosystems.enable, vec!["python.venv"]);
        assert_eq!(config.ecosystems.disable, vec!["terraform"]);
        assert_eq!(config.exclude.len(), 2);
        assert_eq!(config.keep.min_age.as_deref(), Some("7d"));
        assert_eq!(config.keep.per_ecosystem["rust"].min_age.as_deref(), Some("1d"));
        assert_eq!(config.paths.len(), 2);
        assert_eq!(config.paths[0].pattern, "~/work/**");
    }

    /// A typo in a safety-relevant key must fail loudly. Silently ignoring `excludee` would
    /// leave the user believing a guard is in place that is not.
    /// TOML puts a bare key into whichever table header precedes it, so `exclude` written
    /// below `[ecosystems]` becomes `ecosystems.exclude` and is rejected. The documented
    /// examples put the top-level keys first for this reason.
    #[test]
    fn should_reject_a_top_level_key_written_below_a_table_header() {
        let error = toml::from_str::<ConfigFile>("[ecosystems]\nenable = []\nexclude = [\"x\"]").unwrap_err();
        assert!(error.to_string().contains("exclude"), "{error}");
    }

    #[test]
    fn should_reject_an_unknown_top_level_key() {
        let error = toml::from_str::<ConfigFile>(r#"excludee = ["~/work/**"]"#).unwrap_err();
        assert!(error.to_string().contains("excludee"), "{error}");
    }

    #[test]
    fn should_reject_an_unknown_key_inside_a_section() {
        assert!(toml::from_str::<ConfigFile>("[ecosystems]\nenabled = [\"rust\"]").is_err());
        assert!(toml::from_str::<ConfigFile>("[[paths]]\nmatch = \"x\"\nkeepp = {}").is_err());
    }

    #[test]
    fn should_reject_a_misspelled_keep_scalar() {
        // `min_agee` falls into the flattened per-ecosystem map and fails to deserialize as a
        // table, which is the loud failure the strictness rule asks for.
        assert!(toml::from_str::<ConfigFile>("[keep]\nmin_agee = \"7d\"").is_err());
    }

    #[test]
    fn an_empty_file_is_valid_and_sets_nothing() {
        let config: ConfigFile = toml::from_str("").expect("an empty file parses");
        assert!(config.exclude.is_empty());
        assert!(config.keep.scalars().min_age.is_none());
    }
}
