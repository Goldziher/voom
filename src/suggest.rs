//! `voom suggest`: what a tree keeps repeating that the catalog does not know about.
//!
//! The catalog covers ecosystems. It does not cover the cache your own tooling writes into
//! every package directory, and it never will — that is what `include` is for (ADR 0004). The
//! gap this closes is that you have to know the directory's name to write the line, and the
//! whole problem with a tool cache is that nobody looks at it. On the machine this was built
//! for, 69 directories named `.alef` held 42.7 GB and nothing in voom could have mentioned
//! them.
//!
//! # The signal
//!
//! A candidate is a directory that **git actually ignores where it sits**. That is ADR 0005's
//! own premise read the other way round: the walker disables every ignore mechanism *because*
//! "build artifacts are exactly what `.gitignore` lists", which is precisely what makes the
//! same file the right thing to rank suggestions by. Somebody has already said, in writing,
//! that this directory is not source.
//!
//! "Where it sits" is load-bearing and was learned the hard way. Collecting the *names* that
//! any `.gitignore` in the tree mentions, and then matching directories by name, produced
//! `crates`, `lib`, `packages` and `assets` — one repository ignoring a generated `lib/`
//! marked every hand-written `lib/` in every other repository. A suggestion to delete somebody's
//! source is worse than no suggestion at all, so the name pass is only a cheap pre-filter and
//! every survivor is then checked against the `.gitignore` files that actually govern it.
//!
//! It stays a suggestion and never a rule, because `.gitignore` also lists `.env`, editor
//! state and local scratch data — the exact reason ADR 0002 refuses to delete on it. Nothing
//! here removes anything; the output is a line to paste once you agree with it.
//!
//! # The cost
//!
//! Two traversals and a size accounting of the survivors. The first pass reads the tree's
//! `.gitignore` files, the second collects the directories they name; splitting them is what
//! keeps memory bounded by the number of *matching* directories rather than by every directory
//! on the disk. This is an explicit command and is allowed to be slow; a sweep does none of it.

use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::{WalkBuilder, WalkState};

use crate::caches::CacheRoots;
use crate::catalog::CATALOG;
use crate::classify::is_dependency_dir;
use crate::scan::NEVER_DESCEND;
use crate::size;

/// How many occurrences a name needs before it is worth mentioning.
///
/// One is an accident and two is a coincidence; a tool that writes a cache into every package
/// directory produces many. Below this the output is noise, and noise in a suggestion is worse
/// than silence because the reader has no way to check it cheaply.
const MIN_OCCURRENCES: usize = 3;

/// One repeated, unclaimed directory name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// The directory name, as it appears on disk.
    pub name: OsString,
    /// Where it was found. Sorted, and every one of them was measured.
    pub paths: Vec<PathBuf>,
    /// What they hold together.
    pub bytes: u64,
}

impl Suggestion {
    /// The `voom.toml` line that would sweep these.
    #[must_use]
    pub fn include_line(&self) -> String {
        format!("include = [\"{}\"]", Path::new(&self.name).display())
    }
}

/// Finds repeated directory names that no catalog entry claims.
///
/// Ranked by total size, largest first, since the reader is deciding what is worth their
/// attention rather than enumerating a tree.
#[must_use]
pub fn suggest(roots: &[PathBuf], one_file_system: bool, jobs: Option<usize>) -> Vec<Suggestion> {
    let ignored = ignored_names(roots, one_file_system, jobs);
    if ignored.is_empty() {
        return Vec::new();
    }

    let mut rules = Rules::default();
    let mut found: BTreeMap<OsString, Vec<PathBuf>> = BTreeMap::new();
    for (name, path) in matching_directories(roots, &ignored, one_file_system, jobs) {
        if rules.ignores(&path) {
            found.entry(name).or_default().push(path);
        }
    }

    let mut suggestions: Vec<Suggestion> = found
        .into_iter()
        .filter(|(_, paths)| paths.len() >= MIN_OCCURRENCES)
        .map(|(name, mut paths)| {
            paths.sort();
            let bytes = paths.iter().map(|path| size::measure(path)).sum();
            Suggestion { name, paths, bytes }
        })
        .collect();
    suggestions.sort_by(|left, right| right.bytes.cmp(&left.bytes).then_with(|| left.name.cmp(&right.name)));
    suggestions
}

/// Every directory name a `.gitignore` in the tree names outright.
///
/// Only literal directory patterns count. A glob is a rule about shapes rather than a name, and
/// a negation says the opposite of what is wanted, so both are passed over — this is looking for
/// somebody having written down a name, not for a general ignore engine.
fn ignored_names(roots: &[PathBuf], one_file_system: bool, jobs: Option<usize>) -> HashSet<OsString> {
    let names = std::sync::Mutex::new(HashSet::new());
    walk(roots, one_file_system, jobs, |entry, is_dir| {
        if is_dir {
            return WalkState::Continue;
        }
        if entry.file_name() != OsStr::new(".gitignore") {
            return WalkState::Continue;
        }
        if let Ok(contents) = std::fs::read_to_string(entry.path())
            && let Ok(mut names) = names.lock()
        {
            names.extend(literal_directory_names(&contents));
        }
        WalkState::Continue
    });
    names.into_inner().unwrap_or_default()
}

/// Parses the directory names out of one `.gitignore`.
fn literal_directory_names(contents: &str) -> Vec<OsString> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        // A trailing `/` is what makes the pattern mean a directory, which is the only kind of
        // thing an `include` can usefully name.
        .filter_map(|line| line.strip_suffix('/'))
        // `**/name/` and `/name/` are the same statement about a name, so both are unwrapped.
        .map(|line| line.trim_start_matches("**/").trim_start_matches('/'))
        .filter(|name| !name.is_empty() && !name.contains('/') && !name.contains(['*', '?', '[']))
        .map(OsString::from)
        .collect()
}

/// Every directory in the tree whose name one of those `.gitignore`s listed.
fn matching_directories(
    roots: &[PathBuf],
    ignored: &HashSet<OsString>,
    one_file_system: bool,
    jobs: Option<usize>,
) -> Vec<(OsString, PathBuf)> {
    let found = std::sync::Mutex::new(Vec::new());
    walk(roots, one_file_system, jobs, |entry, is_dir| {
        if !is_dir {
            return WalkState::Continue;
        }
        let name = entry.file_name();
        if !ignored.contains(name) || claimed_by_the_catalog(name) {
            return WalkState::Continue;
        }
        if let Ok(mut found) = found.lock() {
            found.push((name.to_os_string(), entry.path().to_path_buf()));
        }
        // Not descended into: a suggestion is about the directory, and anything repeated
        // *inside* one would be counted twice by the sizing below.
        WalkState::Skip
    });
    found.into_inner().unwrap_or_default()
}

/// The `.gitignore` rules governing each directory, built once per directory.
///
/// Rebuilding the matcher for every candidate would re-read the same files dozens of times in a
/// monorepo, and the check runs after the walk rather than inside it, so a plain map is enough —
/// no locking, no sharing.
#[derive(Debug, Default)]
struct Rules {
    by_dir: BTreeMap<PathBuf, Gitignore>,
}

impl Rules {
    /// Whether git ignores this directory where it actually sits.
    fn ignores(&mut self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        if !self.by_dir.contains_key(parent) {
            let rules = Self::build(parent);
            self.by_dir.insert(parent.to_path_buf(), rules);
        }
        self.by_dir
            .get(parent)
            .is_some_and(|rules| rules.matched(path, true).is_ignore())
    }

    /// Collects the `.gitignore` files from the enclosing repository root down to `dir`.
    ///
    /// Outside a repository there is nothing to consult and nothing is ignored — which is the
    /// safe answer, since the whole signal is somebody having committed the statement.
    fn build(dir: &Path) -> Gitignore {
        let mut chain = Vec::new();
        let mut current = Some(dir);
        let mut repository = None;
        while let Some(at) = current {
            chain.push(at.to_path_buf());
            if at.join(".git").exists() {
                repository = Some(at.to_path_buf());
                break;
            }
            current = at.parent();
        }

        let Some(repository) = repository else {
            return Gitignore::empty();
        };
        let mut builder = GitignoreBuilder::new(&repository);
        // Root first, so a nested file can override what an outer one said, which is git's own
        // precedence.
        for at in chain.iter().rev() {
            builder.add(at.join(".gitignore"));
        }
        builder.build().unwrap_or_else(|_| Gitignore::empty())
    }
}

/// Whether the catalog already declares an artifact by this exact name, so suggesting it would
/// tell the reader to configure something voom already does.
fn claimed_by_the_catalog(name: &OsStr) -> bool {
    CATALOG.iter().any(|ecosystem| {
        ecosystem
            .artifacts
            .iter()
            .any(|artifact| OsStr::new(artifact.path.trim_end_matches('/')) == name)
    })
}

/// The shared traversal: the sweep's prunes, without its classification.
///
/// The same subtrees are refused for the same reasons — a suggestion drawn from inside a
/// `node_modules` or a tool cache would be advice to delete somebody's dependencies. Ignore
/// handling stays off here as everywhere else; the `.gitignore` files are *read*, never obeyed,
/// which is the distinction this whole module rests on.
fn walk(
    roots: &[PathBuf],
    one_file_system: bool,
    jobs: Option<usize>,
    visit: impl Fn(&ignore::DirEntry, bool) -> WalkState + Send + Sync,
) {
    for root in roots {
        let caches = CacheRoots::for_root(root, false, &[]);
        let mut builder = WalkBuilder::new(root);
        builder
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .ignore(false)
            .parents(false)
            .hidden(false)
            .follow_links(false)
            .same_file_system(one_file_system);
        if let Some(jobs) = jobs {
            builder.threads(jobs.max(1));
        }

        builder.build_parallel().run(|| {
            Box::new(|result| {
                let Ok(entry) = result else {
                    return WalkState::Continue;
                };
                let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
                let name = entry.file_name();
                if is_dir
                    && (NEVER_DESCEND.iter().any(|never| name == OsStr::new(never))
                        || is_dependency_dir(name)
                        || caches.contains(entry.path()))
                {
                    return WalkState::Skip;
                }
                visit(&entry, is_dir)
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tree;

    fn names(suggestions: &[Suggestion]) -> Vec<String> {
        suggestions
            .iter()
            .map(|suggestion| suggestion.name.to_string_lossy().into_owned())
            .collect()
    }

    fn run(root: &Path) -> Vec<Suggestion> {
        suggest(&[root.to_path_buf()], true, Some(1))
    }

    /// The rules only exist inside a repository, so every fixture needs one. A bare `.git`
    /// directory is enough — nothing here runs git, it only asks where the root is.
    fn repository(root: &Path, gitignore: &str) {
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".gitignore"), gitignore).unwrap();
    }

    #[test]
    fn should_suggest_a_repeated_name_a_gitignore_lists() {
        let fixture = tree(&["a/.mytool/blob", "b/.mytool/blob", "c/.mytool/blob"]);
        repository(fixture.path(), ".mytool/\n");

        assert_eq!(names(&run(fixture.path())), vec![".mytool".to_owned()]);
    }

    /// The signal is somebody having written the name down. Without that it is just a directory.
    #[test]
    fn should_not_suggest_a_name_no_gitignore_names() {
        let fixture = tree(&["a/.mytool/blob", "b/.mytool/blob", "c/.mytool/blob"]);
        repository(fixture.path(), "unrelated/\n");

        assert!(run(fixture.path()).is_empty());
    }

    /// One is an accident and two is a coincidence.
    #[test]
    fn should_not_suggest_a_name_that_barely_repeats() {
        let fixture = tree(&["a/.mytool/blob", "b/.mytool/blob"]);
        repository(fixture.path(), ".mytool/\n");

        assert!(run(fixture.path()).is_empty());
    }

    /// Suggesting `target/` would be advice to configure what voom already does.
    #[test]
    fn should_not_suggest_something_the_catalog_already_claims() {
        let fixture = tree(&["a/target/o", "b/target/o", "c/target/o"]);
        repository(fixture.path(), "target/\n");

        assert!(run(fixture.path()).is_empty());
    }

    /// A glob is a rule about shapes; a negation says the opposite. Neither is a name.
    #[test]
    fn should_read_only_literal_directory_names_out_of_a_gitignore() {
        let parsed =
            literal_directory_names("# comment\n.alef/\n**/.basemind/\n/build/\nbuild-*/\n!keep/\nnotes.txt\n");

        assert_eq!(
            parsed,
            vec![
                OsString::from(".alef"),
                OsString::from(".basemind"),
                OsString::from("build")
            ]
        );
    }

    /// Suggesting anything found inside a dependency directory would be advice to delete
    /// somebody's dependencies.
    #[test]
    fn should_never_look_inside_a_dependency_directory() {
        let fixture = tree(&[
            "node_modules/a/.mytool/blob",
            "node_modules/b/.mytool/blob",
            "node_modules/c/.mytool/blob",
        ]);
        repository(fixture.path(), ".mytool/\n");

        assert!(run(fixture.path()).is_empty());
    }

    /// A candidate is not descended into, so a candidate nested inside another is attributed to
    /// the outer one alone. Counting both would report the same bytes twice and suggest two
    /// `include` lines where one does the job.
    #[test]
    fn should_attribute_nested_candidates_to_the_outer_one() {
        let fixture = tree(&["a/out/.mytool/blob", "b/out/.mytool/blob", "c/out/.mytool/blob"]);
        repository(fixture.path(), "out/\n.mytool/\n");

        assert_eq!(
            names(&run(fixture.path())),
            vec!["out".to_owned()],
            "the outer name is suggested and the inner one is not counted again"
        );
    }

    /// The regression test for the shape that made the first version dangerous. One repository
    /// ignoring a generated `lib/` must not mark the hand-written `lib/` in another — run
    /// against a real workspace, the name-only version suggested `crates`, `lib`, `packages`
    /// and a source repository, which is advice to delete somebody's work.
    #[test]
    fn should_not_suggest_a_directory_another_repository_happens_to_ignore() {
        let fixture = tree(&[
            "generated/a/lib/blob",
            "generated/b/lib/blob",
            "handwritten/x/lib/source.rs",
            "handwritten/y/lib/source.rs",
            "handwritten/z/lib/source.rs",
        ]);
        repository(&fixture.path().join("generated"), "lib/\n");
        repository(&fixture.path().join("handwritten"), "unrelated/\n");

        let suggestions = run(fixture.path());

        assert!(
            suggestions.is_empty(),
            "the two ignored `lib/`s are below the threshold and the three hand-written ones \
             are not ignored at all — suggesting them would be advice to delete source: {:?}",
            names(&suggestions)
        );
    }

    #[test]
    fn should_emit_a_pasteable_include_line() {
        let suggestion = Suggestion {
            name: OsString::from(".alef"),
            paths: Vec::new(),
            bytes: 0,
        };

        assert_eq!(suggestion.include_line(), "include = [\".alef\"]");
    }
}
