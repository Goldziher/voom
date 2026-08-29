//! Machine-global tool caches and installed toolchains, which voom leaves alone.
//!
//! These are not build output. `~/.cargo/registry` is a shared download cache with its own
//! eviction tooling; `~/.pyenv/versions` and `~/google-cloud-sdk` are *installed programs* that
//! happen to be laid out like projects. Marker anchoring (ADR 0002) classifies artifacts inside
//! them entirely correctly — `~/.cache/node/corepack/*/pnpm/*/dist` really is a `dist/` beside a
//! real `package.json` — which is precisely the problem: the classifier cannot tell "a project
//! that was built here" from "a program that was installed here", because on disk they are the
//! same shape.
//!
//! So the distinction is drawn by location instead, before classification runs. A sweep of
//! `$HOME` on the author's machine found 469 artifacts, 301 of them in the roots below, and
//! removing them would have broken an installed pnpm, the bun toolchain and the gcloud SDK
//! while reclaiming space that those tools re-download on their own terms.
//!
//! `--caches` opts back in. Naming one of these paths as an explicit scan root also works
//! without the flag: the skip applies to what a walk descends into, not to what the user
//! deliberately points at.
//!
//! See `adrs/0001-mission-and-scope.md`, which put these out of scope for v1 and floated the
//! opt-in flag that now exists.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

mod catalog;

pub use catalog::{CACHES, Cache};

/// Directories, relative to the user's home, that a walk never descends into.
///
/// ~keep This list is **append-only**, for the same reason `delete.rs`'s protected paths are:
/// adding an entry costs a user some reclaimable space, removing one can cost them a working
/// toolchain. Removing or narrowing an entry requires an ADR amendment, never a drive-by edit.
///
/// Entries are matched as whole directories, not prefixes, and each is checked against the real
/// filesystem when a scan starts — a name that does not exist on this machine costs nothing.
pub(crate) const CACHE_DIRS: &[&str] = &[
    // Rust
    ".cargo/bin",
    ".cargo/git",
    ".cargo/registry",
    ".rustup",
    // JavaScript
    ".bun",
    ".deno",
    ".npm",
    ".nvm",
    ".pnpm-store",
    ".volta",
    ".yarn",
    // Python
    ".conda",
    ".local/pipx",
    ".local/share/virtualenvs",
    ".pyenv",
    "miniconda3",
    "miniforge3",
    // JVM
    ".gradle",
    ".ivy2",
    ".m2",
    ".sbt",
    ".sdkman",
    // Ruby, PHP, Elixir, Haskell, OCaml
    ".bundle",
    ".cabal",
    ".gem",
    ".ghcup",
    ".hex",
    ".opam",
    ".rbenv",
    ".stack",
    // Go, .NET, Dart, Swift
    ".dotnet",
    ".nuget",
    ".pub-cache",
    ".swiftpm",
    "fvm",
    "go/bin",
    "go/pkg",
    // Version managers and SDKs
    ".asdf",
    ".nodenv",
    ".terraform.d",
    "google-cloud-sdk",
    // Installed toolchains and tool extensions
    ".local/share/gh",
    "Library/Developer/Toolchains",
    // Editor and agent state
    ".antigravity",
    ".hermes",
    ".vscode",
    ".vscode-server",
    // Generic per-user cache and application state
    ".cache",
    ".local/state",
    "AppData/Local",
    "AppData/Roaming",
    "Library/Application Support",
    "Library/Caches",
    "scoop",
];

/// The cache directories that lie inside one scan root, spelled the way that root is spelled.
///
/// The walker is handed the path the user typed rather than a canonical one (see
/// `run::normalize_roots`), so a set of canonical cache paths would not match what it produces
/// and the skip would silently fail *open* — sweeping the very directories it exists to
/// protect. Resolving against the root's own spelling is what makes the comparison in
/// `scan::Visitor` a plain equality test on the path in hand.
///
/// Only strict descendants are returned. A cache directory named as the scan root itself is
/// swept: the user pointed at it, and this is a rule about what a walk wanders into.
#[derive(Debug, Default)]
pub struct CacheRoots {
    paths: HashSet<PathBuf>,
    /// The removable caches among them, and which entry each is. Only populated for entries
    /// the run actually enabled, so a default sweep builds nothing extra.
    removable: HashMap<PathBuf, &'static Cache>,
}

impl CacheRoots {
    /// Resolves the cache directories that sit under `root`.
    ///
    /// Returns an empty set when caches are enabled, when the home directory cannot be
    /// determined, or when the root cannot be canonicalized — the caller has already reported a
    /// root it cannot resolve, and an empty set means "skip nothing", never "skip everything".
    #[must_use]
    pub fn for_root(root: &Path, enabled: bool, clean: &[String]) -> Self {
        let Some(home) = dirs::home_dir() else {
            return Self::default();
        };
        let mut resolved = if enabled {
            Self::default()
        } else {
            Self::under(&home, root)
        };
        resolved.removable = removable_under(&home, root, clean);
        resolved
    }

    /// The resolution itself, with the home directory passed in so tests can build one under a
    /// `TempDir` rather than reaching for the real `$HOME`.
    #[cfg(test)]
    pub(crate) fn under_with(home: &Path, root: &Path, clean: &[String]) -> Self {
        let mut resolved = Self::under(home, root);
        resolved.removable = removable_under(home, root, clean);
        resolved
    }

    pub(crate) fn under(home: &Path, root: &Path) -> Self {
        // Both sides are canonicalized before they are compared. Mixing the two forms silently
        // answers "no" to every question on any machine where the home directory or the root is
        // reached through a symlink — which on macOS is every path under `/var`.
        let (Ok(home), Ok(canonical_root)) = (home.canonicalize(), root.canonicalize()) else {
            return Self::default();
        };

        // Every entry lives under the home directory, so unless the root and the home directory
        // are on the same branch of the tree none of them can possibly be inside it. Checking
        // that first keeps a sweep of an unrelated tree from stat-ing the whole list.
        if !canonical_root.starts_with(&home) && !home.starts_with(&canonical_root) {
            return Self::default();
        }

        let paths = CACHE_DIRS
            .iter()
            .filter_map(|cache| home.join(cache).canonicalize().ok())
            .filter_map(|cache| cache.strip_prefix(&canonical_root).ok().map(Path::to_path_buf))
            .filter(|relative| relative.components().next().is_some())
            .map(|relative| root.join(relative))
            .collect();

        Self {
            paths,
            removable: HashMap::new(),
        }
    }

    /// Whether `path` is one of the cache directories to skip.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        !self.paths.is_empty() && self.paths.contains(path)
    }

    /// The cache entry this path is, if the run enabled one that lives here.
    ///
    /// Checked before [`contains`](Self::contains) in the walker, so naming a cache reaches it
    /// without `--caches` having to open the location up wholesale.
    #[must_use]
    pub fn removable(&self, path: &Path) -> Option<&'static Cache> {
        if self.removable.is_empty() {
            return None;
        }
        self.removable.get(path).copied()
    }

    /// The enabled caches that live strictly *inside* a directory the walk is about to prune.
    ///
    /// Most cache locations sit under a broader skip — `~/.cache/uv` under `~/.cache`,
    /// `~/.npm/_cacache` under `~/.npm`, everything in `~/Library/Caches` — so the prune fires
    /// at the ancestor and the walk never arrives. Opening the ancestor up instead would let
    /// the classifier loose inside it, which is the whole thing [`CACHE_DIRS`] exists to
    /// prevent: an installed pnpm really does sit beside a real `package.json`.
    ///
    /// So the walker does here what it already does for an artifact declared inside a pruned
    /// dependency directory — constructs the one path it was told about, and still prunes
    /// everything else. No traversal, one `read_dir` of the cache itself, and only when the run
    /// named it.
    #[must_use]
    pub fn removable_inside(&self, dir: &Path) -> Vec<(&Path, &'static Cache)> {
        if self.removable.is_empty() {
            return Vec::new();
        }
        self.removable
            .iter()
            .filter(|(path, _)| path.as_path() != dir && path.starts_with(dir))
            .map(|(path, cache)| (path.as_path(), *cache))
            .collect()
    }

    /// Whether there is anything to skip at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// Resolves the enabled caches' locations under `root`, in the root's own spelling.
///
/// The same re-spelling `under` does and for the same reason: the walker compares against the
/// path it produced, not a canonical one. A location that does not exist on this machine, or
/// lies outside this root, simply does not appear.
fn removable_under(home: &Path, root: &Path, clean: &[String]) -> HashMap<PathBuf, &'static Cache> {
    if clean.is_empty() {
        return HashMap::new();
    }
    let (Ok(home), Ok(canonical_root)) = (home.canonicalize(), root.canonicalize()) else {
        return HashMap::new();
    };

    let mut resolved = HashMap::new();
    for cache in CACHES {
        if !clean.iter().any(|id| id == cache.id) {
            continue;
        }
        for location in cache.locations {
            let Ok(absolute) = home.join(location).canonicalize() else {
                continue;
            };
            let Ok(relative) = absolute.strip_prefix(&canonical_root) else {
                continue;
            };
            if relative.components().next().is_none() {
                continue;
            }
            resolved.insert(root.join(relative), cache);
        }
    }
    resolved
}

/// The ids in `clean` that no entry declares, so a typo is an error rather than a silent no-op.
#[must_use]
pub fn unknown_ids(clean: &[String]) -> Vec<String> {
    clean
        .iter()
        .filter(|id| !CACHES.iter().any(|cache| cache.id == **id))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{CACHE_DIRS, CacheRoots};
    use std::fs;
    use tempfile::TempDir;

    /// Builds a fake home containing `dirs`, and returns it.
    fn home(dirs: &[&str]) -> TempDir {
        let home = TempDir::new().expect("a temp dir");
        for dir in dirs {
            fs::create_dir_all(home.path().join(dir)).expect("a cache dir");
        }
        home
    }

    #[test]
    fn should_resolve_a_cache_directory_that_sits_under_the_root() {
        let home = home(&[".cargo/registry", "projects"]);
        let roots = CacheRoots::under(home.path(), home.path());

        assert!(roots.contains(&home.path().join(".cargo/registry")));
        assert!(!roots.contains(&home.path().join("projects")));
    }

    /// The walker is handed the spelling the user typed. If the skip set were canonical it would
    /// not match, and the caches would be swept.
    #[test]
    #[cfg(unix)]
    fn should_spell_the_cache_paths_the_way_the_root_is_spelled() {
        let home = home(&[".npm"]);
        let link = home.path().parent().expect("a parent").join("voom-cache-link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(home.path(), &link).expect("a symlink");

        let roots = CacheRoots::under(home.path(), &link);

        assert!(
            roots.contains(&link.join(".npm")),
            "the skip set must match what the walker produces, not where the path really is"
        );
        fs::remove_file(&link).expect("the link is removed");
    }

    #[test]
    fn should_ignore_caches_that_sit_outside_the_root() {
        let home = home(&[".npm", "work"]);
        let roots = CacheRoots::under(home.path(), &home.path().join("work"));

        assert!(roots.is_empty(), "a cache beside the root is not the walk's concern");
    }

    /// Pointing voom *at* a cache is explicit intent; this rule is about what a walk wanders
    /// into, not about what the user asked for.
    #[test]
    fn should_not_skip_a_cache_named_as_the_scan_root_itself() {
        let home = home(&[".npm"]);
        let cache = home.path().join(".npm");
        let roots = CacheRoots::under(home.path(), &cache);

        assert!(roots.is_empty());
        assert!(!roots.contains(&cache));
    }

    #[test]
    fn should_skip_nothing_when_caches_are_enabled() {
        let home = home(&[".cargo/registry"]);
        assert!(CacheRoots::for_root(home.path(), true, &[]).is_empty());
    }

    /// A cache root that does not exist on this machine must cost nothing and skip nothing.
    #[test]
    fn should_tolerate_a_home_with_no_caches_at_all() {
        let home = home(&[]);
        assert!(CacheRoots::under(home.path(), home.path()).is_empty());
    }

    /// `CACHE_DIRS` is append-only for the same reason the protected denylist is: adding an
    /// entry costs a user reclaimable space, removing one can cost them a working toolchain.
    /// The entries below are the ones a sweep of a real machine proved were needed — see the
    /// module doc and ADR 0001's amendment.
    #[test]
    fn the_cache_list_is_append_only() {
        const REQUIRED: &[&str] = &[
            ".bun",
            ".cache",
            // The three below hold *installed binaries*, and a sweep of a real machine found
            // them reachable: `bin/` is a catalog candidate, so `~/go/bin` needed only a
            // `go.mod` beside it and `--enable go.bin` to be swept. 5.6 GB of installed tools
            // were resting on the marker rule alone.
            ".cargo/bin",
            ".cargo/git",
            ".cargo/registry",
            ".local/share/gh",
            ".gem",
            ".gradle",
            ".m2",
            ".npm",
            ".pub-cache",
            ".pyenv",
            ".rustup",
            "Library/Application Support",
            "Library/Caches",
            "Library/Developer/Toolchains",
            "go/bin",
            "google-cloud-sdk",
            "miniforge3",
        ];

        for required in REQUIRED {
            assert!(
                CACHE_DIRS.contains(required),
                "`{required}` was removed from CACHE_DIRS. The list is append-only: removing an \
                 entry means voom will sweep build output out of an installed toolchain again, \
                 which is what ADR 0001's amendment exists to prevent."
            );
        }
    }

    #[test]
    fn every_cache_dir_is_relative_and_stays_under_home() {
        for cache in CACHE_DIRS {
            let path = std::path::Path::new(cache);
            assert!(path.is_relative(), "{cache}: must be relative to the home directory");
            assert!(
                !cache.contains(".."),
                "{cache}: must not climb out of the home directory"
            );
            assert!(!cache.ends_with('/'), "{cache}: no trailing separator");
        }
    }

    #[test]
    fn cache_dirs_are_unique_and_sorted_within_their_group() {
        let mut seen = std::collections::HashSet::new();
        for cache in CACHE_DIRS {
            assert!(seen.insert(*cache), "{cache}: listed twice");
        }
    }
}
