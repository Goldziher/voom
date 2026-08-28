//! Finding and reading configuration files.

use std::path::{Path, PathBuf};

use super::schema::ConfigFile;
use crate::error::{Error, Result};

/// The per-directory configuration file name.
pub const CONFIG_FILE_NAME: &str = "voom.toml";

/// One loaded file, with where it came from.
///
/// The source path is carried all the way through the merge so `voom config show` can say which
/// file each value came from — a hierarchical merge nobody can inspect is one nobody can trust.
#[derive(Debug)]
pub struct Layer {
    /// The file this layer was read from.
    pub source: PathBuf,
    /// What it said.
    pub file: ConfigFile,
}

/// The layers that apply to a run, in ascending precedence.
#[derive(Debug, Default)]
pub struct Sources {
    /// User config, then each `voom.toml` from the scan root downward.
    pub layers: Vec<Layer>,
    /// Whether `--config` named these, in which case no further discovery happens.
    pub explicit: bool,
}

/// Where the user's own configuration lives.
///
/// `$XDG_CONFIG_HOME` if set, otherwise `~/.config`.
#[must_use]
pub fn user_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))?;
    Some(base.join("voom").join("config.toml"))
}

/// Reads and parses one file.
///
/// # Errors
///
/// [`Error::Config`] if the file cannot be read or does not parse. Both name the path, because
/// a parse error that does not say which of several merged files was wrong is not actionable.
pub fn read(path: &Path) -> Result<Layer> {
    let text = std::fs::read_to_string(path).map_err(|source| Error::Config {
        path: path.to_path_buf(),
        reason: source.to_string(),
    })?;
    let file: ConfigFile = toml::from_str(&text).map_err(|source| Error::Config {
        path: path.to_path_buf(),
        reason: source.to_string(),
    })?;
    Ok(Layer {
        source: path.to_path_buf(),
        file,
    })
}

/// Collects the layers that sit *above* a scan root: the user's own configuration, or the one
/// file `--config` named.
///
/// The per-directory hierarchy from the root downwards belongs to the resolver, which reads
/// each `voom.toml` exactly once as it descends.
///
/// `explicit` is `--config`, which replaces the user config and the discovered hierarchy
/// entirely — one file, no surprises, which is what an explicit flag should mean.
///
/// # Errors
///
/// [`Error::Config`] if a file exists but cannot be read or parsed. A file that simply is not
/// there is not an error; a file that is there and is broken always is.
pub fn discover(root: &Path, explicit: Option<&Path>) -> Result<Sources> {
    if let Some(path) = explicit {
        return Ok(Sources {
            layers: vec![read(path)?],
            explicit: true,
        });
    }

    let mut layers = Vec::new();
    if let Some(path) = user_config_path()
        && path.is_file()
    {
        layers.push(read(&path)?);
    }

    // The scan root's own `voom.toml` is deliberately *not* read here. The resolver walks the
    // hierarchy starting at the root, so reading it here as well would apply it twice — which
    // is harmless for scalars but duplicates every `exclude` and every `[[paths]]` rule.
    let _ = root;

    Ok(Sources {
        layers,
        explicit: false,
    })
}

/// Reads the `voom.toml` in a directory, if there is one.
///
/// Used to extend the hierarchy below the scan root. Returns `Ok(None)` when there is no file.
///
/// # Errors
///
/// [`Error::Config`] if the file exists but will not parse.
pub fn read_in(dir: &Path) -> Result<Option<Layer>> {
    let path = dir.join(CONFIG_FILE_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    read(&path).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tree;

    #[test]
    fn should_read_a_valid_file() {
        let fixture = tree(&["voom.toml"]);
        std::fs::write(fixture.path().join("voom.toml"), "exclude = [\"**/fixtures/**\"]").unwrap();
        let layer = read(&fixture.path().join("voom.toml")).expect("it parses");
        assert_eq!(layer.file.exclude, vec!["**/fixtures/**"]);
        assert_eq!(layer.source, fixture.path().join("voom.toml"));
    }

    /// A broken config must name itself. With several files merged, "invalid TOML" alone does
    /// not tell the user which one to fix.
    #[test]
    fn should_name_the_file_in_a_parse_error() {
        let fixture = tree(&["voom.toml"]);
        std::fs::write(fixture.path().join("voom.toml"), "exclude = [").unwrap();
        let error = read(&fixture.path().join("voom.toml")).unwrap_err();
        assert!(error.to_string().contains("voom.toml"), "{error}");
    }

    #[test]
    fn should_treat_a_missing_file_as_no_configuration() {
        let fixture = tree(&["Cargo.toml"]);
        let sources = discover(fixture.path(), None).expect("discovery succeeds");
        assert!(
            sources
                .layers
                .iter()
                .all(|layer| layer.source != fixture.path().join("voom.toml"))
        );
        assert!(read_in(fixture.path()).expect("no file is not an error").is_none());
    }

    /// The root's own `voom.toml` belongs to the resolver's descent, not to discovery — loading
    /// it in both places would apply every `exclude` and `[[paths]]` rule twice.
    #[test]
    fn should_leave_the_root_voom_toml_to_the_resolver() {
        let fixture = tree(&["voom.toml"]);
        std::fs::write(fixture.path().join("voom.toml"), "[keep]\nmin_age = \"7d\"").unwrap();
        let sources = discover(fixture.path(), None).expect("discovery succeeds");
        assert!(
            sources
                .layers
                .iter()
                .all(|layer| layer.source != fixture.path().join("voom.toml"))
        );

        let layer = read_in(fixture.path()).expect("it parses").expect("a layer");
        assert_eq!(layer.file.keep.min_age.as_deref(), Some("7d"));
    }

    /// `--config` means one file and no surprises.
    #[test]
    fn should_replace_the_hierarchy_when_a_config_is_named_explicitly() {
        let fixture = tree(&["voom.toml", "explicit.toml"]);
        std::fs::write(fixture.path().join("voom.toml"), "[keep]\nmin_age = \"7d\"").unwrap();
        std::fs::write(fixture.path().join("explicit.toml"), "[keep]\nmin_age = \"1d\"").unwrap();

        let explicit = fixture.path().join("explicit.toml");
        let sources = discover(fixture.path(), Some(&explicit)).expect("discovery succeeds");
        assert_eq!(sources.layers.len(), 1);
        assert_eq!(sources.layers[0].source, explicit);
    }

    #[test]
    fn should_fail_when_an_explicit_config_does_not_exist() {
        let fixture = tree(&["Cargo.toml"]);
        let missing = fixture.path().join("nope.toml");
        assert!(discover(fixture.path(), Some(&missing)).is_err());
    }
}
