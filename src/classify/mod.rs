//! Marker anchoring: candidate + markers -> verdict.
//!
//! This is the rail the whole tool rests on. A directory is an artifact **only** when a marker
//! file proves its ecosystem at the position that ecosystem declares. The name alone is never
//! enough — a survey of one workspace found 41 directories named `bin`, 20 named `vendor` and
//! 12 named `obj`, each build output in one ecosystem and hand-written source in another.
//! See `adrs/0002-marker-anchored-classification.md`.
//!
//! Two things make this cheap enough to run over `$HOME`:
//!
//! - **Candidates are rejected by name first.** [`NameMatcher`] answers "could this entry be an
//!   artifact at all?" with a hash lookup over the entry's raw bytes. The overwhelming majority
//!   of entries stop there, having allocated nothing.
//! - **Directory facts are probed once and memoized.** A single `read_dir` of an anchor
//!   directory answers the marker question for *every* ecosystem at once, packed into a `u32`
//!   bitmask, and answers `voom.toml` discovery (ADR 0004) from the same pass. Only anchor
//!   directories are ever probed, and each is probed at most once per run — so an `Ancestor(n)`
//!   lookup in a deep tree does not re-check the same parents for every sibling.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::catalog::{self, Artifact, CATALOG};
use crate::error::Result;
use crate::names::NameMatcher;

mod selection;
mod verdict;

pub use selection::ArtifactId;
pub use selection::Selection;
pub use verdict::{Finding, Provenance, SkipReason, Verdict};

/// The per-directory configuration file discovered during the walk (ADR 0004).
pub const CONFIG_FILE_NAME: &str = "voom.toml";

/// What one `read_dir` of a directory told us, cached for the rest of the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirFacts {
    /// Bit `i` set means `CATALOG[i]`'s marker glob matched a name in this directory.
    markers: u32,
    /// Whether the directory could be listed at all. An unreadable directory proves nothing,
    /// which means its candidates survive — the safe direction.
    readable: bool,
}

impl DirFacts {
    const UNREADABLE: Self = Self {
        markers: 0,
        readable: false,
    };
}

/// Resolves candidates against markers, memoizing what it learns about each directory.
#[derive(Debug)]
pub struct Classifier {
    artifacts: NameMatcher<ArtifactId>,
    markers: NameMatcher<u8>,
    selection: Selection,
    /// `(dependency directory name, artifact declared inside it)`.
    nested_in_dependencies: Vec<(&'static str, ArtifactId)>,
    facts: RwLock<HashMap<PathBuf, DirFacts>>,
    probes: AtomicUsize,
}

impl Classifier {
    /// Indexes the catalog for the given selection.
    ///
    /// # Errors
    ///
    /// [`crate::error::Error::CatalogPattern`] if a catalog pattern will not compile. That is a
    /// bug in voom rather than in the user's tree, and the catalog tests exist to prevent it.
    pub fn new(selection: Selection) -> Result<Self> {
        let mut artifact_patterns = Vec::new();
        let mut marker_patterns = Vec::new();
        let mut nested_in_dependencies = Vec::new();

        for (index, ecosystem) in CATALOG.iter().enumerate() {
            let Ok(ecosystem_index) = u8::try_from(index) else {
                continue;
            };
            for marker in ecosystem.markers {
                marker_patterns.push((*marker, ecosystem_index));
            }
            for (offset, artifact) in ecosystem.artifacts.iter().enumerate() {
                let Ok(artifact_index) = u8::try_from(offset) else {
                    continue;
                };
                // Candidates are keyed on the artifact's *last* segment; any leading segments
                // are verified against the entry's ancestors once a name matches.
                let last = artifact
                    .path
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(artifact.path);
                let id = ArtifactId {
                    ecosystem: ecosystem_index,
                    artifact: artifact_index,
                };
                artifact_patterns.push((last, id));

                if artifact.depth() > 1
                    && let Some(first) = artifact.segments().next()
                    && let Some(dependency) = catalog::DEPENDENCY_DIRS.iter().find(|dir| **dir == first)
                {
                    nested_in_dependencies.push((*dependency, id));
                }
            }
        }

        Ok(Self {
            artifacts: NameMatcher::build(artifact_patterns)?,
            markers: NameMatcher::build(marker_patterns)?,
            selection,
            nested_in_dependencies,
            facts: RwLock::new(HashMap::new()),
            probes: AtomicUsize::new(0),
        })
    }

    /// How many directories have actually been listed.
    ///
    /// Marker resolution is the one place classification touches the filesystem, so this is
    /// the number that says whether memoization is working: it must stay proportional to the
    /// number of *anchor* directories, not to the number of candidates or entries.
    #[must_use]
    pub fn probe_count(&self) -> usize {
        self.probes.load(Ordering::Relaxed)
    }

    /// The selection this classifier was built with.
    #[must_use]
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Artifacts the catalog declares *inside* a dependency directory of this name.
    ///
    /// ADR 0001 keeps dependency caches out of scope and ADR 0005 prunes them at the directory
    /// level, but two catalog entries name build output that lives inside one — Ruby's
    /// `vendor/bundle/` and Julia's `deps/build/`. Without this the walker would prune the
    /// parent and those two entries would be permanently unreachable. The scanner checks these
    /// specific paths and then prunes as usual, so nothing else below is ever walked.
    /// Returns an empty `Vec` — which does not allocate — for the overwhelming majority of
    /// dependency directories, which declare nothing.
    #[must_use]
    pub fn artifacts_inside(&self, name: &OsStr) -> Vec<ArtifactId> {
        self.nested_in_dependencies
            .iter()
            .filter(|(dir, _)| name == OsStr::new(*dir))
            .map(|(_, id)| *id)
            .collect()
    }

    /// Decides what one directory entry is.
    ///
    /// `root` bounds the search: a marker above the scan root never proves an artifact inside
    /// it, because voom must not justify a deletion with evidence from a tree it was not
    /// pointed at.
    pub fn classify(&self, root: &Path, path: &Path, is_dir: bool) -> Verdict {
        let Some(name) = path.file_name() else {
            return Verdict::NotACandidate;
        };

        // `Vec::new()` does not allocate; the first push only happens for a real candidate,
        // which is a small fraction of the entries the walker produces.
        let mut candidates: Vec<ArtifactId> = Vec::new();
        self.artifacts.for_each_match(name, |id| candidates.push(id));
        if candidates.is_empty() {
            return Verdict::NotACandidate;
        }
        candidates.sort_unstable();

        // A candidate name can belong to several ecosystems (`target/` is Rust's, Maven's and
        // sbt's; `bin/` is Go's and .NET's). Only one of them usually applies, so when none
        // proves out we report the *most useful* reason rather than the first: an artifact
        // whose ecosystem is genuinely present but which is off by default outranks a missing
        // marker for an ecosystem that was never plausible here.
        let mut best: Option<(u8, SkipReason)> = None;
        let mut record = |rank: u8, reason: SkipReason| {
            if best.as_ref().is_none_or(|(existing, _)| rank < *existing) {
                best = Some((rank, reason));
            }
        };

        for id in candidates {
            let artifact = id.artifact();
            if artifact.requires_dir() && !is_dir {
                continue;
            }
            let Some(anchor_dir) = anchor_dir(path, artifact) else {
                continue;
            };
            if !anchor_dir.starts_with(root) {
                continue;
            }
            // An ecosystem the user did not select is not worth a directory listing to
            // disprove, so this check comes before any probing.
            if !self.selection.ecosystem_enabled(id) {
                record(
                    RANK_ECOSYSTEM_OFF,
                    SkipReason::NotEnabled {
                        spec: id.spec(),
                        note: artifact.note,
                    },
                );
                continue;
            }

            match (self.prove(root, anchor_dir, id), self.selection.is_enabled(id)) {
                (Proof::Found(marker_dir), true) => {
                    return Verdict::Artifact(Finding {
                        path: path.to_path_buf(),
                        provenance: Provenance::Anchored {
                            artifact: id,
                            marker_dir,
                        },
                    });
                }
                (Proof::Found(_), false) => {
                    record(
                        RANK_PROVEN_OPT_IN,
                        SkipReason::NotEnabled {
                            spec: id.spec(),
                            note: artifact.note,
                        },
                    );
                }
                (Proof::Missing, true) => record(
                    RANK_NO_MARKER,
                    SkipReason::NoMarker {
                        ecosystem: id.ecosystem().id,
                        markers: id.ecosystem().markers,
                    },
                ),
                (Proof::Missing, false) => record(
                    RANK_UNPROVEN_OPT_IN,
                    SkipReason::NotEnabled {
                        spec: id.spec(),
                        note: artifact.note,
                    },
                ),
                (Proof::Unreadable, _) => record(RANK_UNREADABLE, SkipReason::Unreadable),
            }
        }

        best.map_or(Verdict::NotACandidate, |(_, reason)| Verdict::Skip(reason))
    }

    /// Looks for a marker at the artifact's declared anchor position.
    fn prove(&self, root: &Path, anchor_dir: &Path, id: ArtifactId) -> Proof {
        let bit = 1_u32 << id.ecosystem;
        let levels = id.artifact().anchor_in(id.ecosystem()).levels();
        let mut dir = anchor_dir;
        let mut unreadable = false;

        for _ in 0..=levels {
            let facts = self.facts(dir);
            if facts.markers & bit != 0 {
                return Proof::Found(dir.to_path_buf());
            }
            unreadable |= !facts.readable;
            if dir == root {
                break;
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }

        if unreadable { Proof::Unreadable } else { Proof::Missing }
    }

    /// Everything we know about a directory, listing it exactly once per run.
    fn facts(&self, dir: &Path) -> DirFacts {
        if let Ok(cache) = self.facts.read()
            && let Some(facts) = cache.get(dir)
        {
            return *facts;
        }
        let facts = self.probe(dir);
        if let Ok(mut cache) = self.facts.write() {
            cache.insert(dir.to_path_buf(), facts);
        }
        facts
    }

    /// One `read_dir`, answering every marker question and `voom.toml` discovery at once.
    fn probe(&self, dir: &Path) -> DirFacts {
        self.probes.fetch_add(1, Ordering::Relaxed);
        let Ok(entries) = std::fs::read_dir(dir) else {
            return DirFacts::UNREADABLE;
        };
        let mut facts = DirFacts {
            markers: 0,
            readable: true,
        };
        for entry in entries.flatten() {
            self.markers
                .for_each_match(&entry.file_name(), |index| facts.markers |= 1_u32 << index);
        }
        facts
    }
}

/// Skip-reason precedence, lowest number reported. The ordering is the order of usefulness to
/// somebody asking "why didn't you take that?".
const RANK_PROVEN_OPT_IN: u8 = 0;
const RANK_NO_MARKER: u8 = 1;
const RANK_UNPROVEN_OPT_IN: u8 = 2;
const RANK_ECOSYSTEM_OFF: u8 = 3;
const RANK_UNREADABLE: u8 = 4;

/// The outcome of looking for a marker.
enum Proof {
    Found(PathBuf),
    Missing,
    Unreadable,
}

/// The directory an artifact anchors against: the candidate path with the artifact's own
/// relative path stripped off.
///
/// The artifact's last segment matched the entry's name, so this verifies the segments above it
/// and walks up. Leading segments are literals — `leading_artifact_segments_are_literal` in the
/// catalog tests keeps them that way, which is what lets this be a byte comparison.
fn anchor_dir<'p>(path: &'p Path, artifact: &Artifact) -> Option<&'p Path> {
    let mut dir = path.parent()?;
    for segment in artifact.path.trim_end_matches('/').rsplit('/').skip(1) {
        if dir.file_name()?.as_encoded_bytes() != segment.as_bytes() {
            return None;
        }
        dir = dir.parent()?;
    }
    Some(dir)
}

/// Whether a directory name is one of the dependency caches ADR 0001 puts out of scope.
#[must_use]
pub fn is_dependency_dir(name: &OsStr) -> bool {
    catalog::DEPENDENCY_DIRS.iter().any(|dir| name == OsStr::new(dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tree;

    fn classifier() -> Classifier {
        Classifier::new(Selection::all()).unwrap()
    }

    /// Classifies a path inside the fixture, inferring whether it is a directory from disk.
    fn verdict(classifier: &Classifier, root: &Path, relative: &str) -> Verdict {
        let path = root.join(relative);
        let is_dir = path.is_dir();
        classifier.classify(root, &path, is_dir)
    }

    fn assert_artifact(verdict: &Verdict, ecosystem: &str, artifact: &str) {
        match verdict {
            Verdict::Artifact(finding) => {
                let id = finding.artifact().expect("a marker proved it");
                assert_eq!(id.ecosystem().id, ecosystem);
                assert_eq!(id.artifact().path, artifact);
            }
            other => panic!("expected an artifact verdict, got {other:?}"),
        }
    }

    #[test]
    fn should_classify_target_as_rust_when_cargo_toml_is_beside_it() {
        let fixture = tree(&["Cargo.toml", "target/"]);
        let classifier = classifier();
        let verdict = verdict(&classifier, fixture.path(), "target");
        assert_artifact(&verdict, "rust", "target/");
        match verdict {
            Verdict::Artifact(finding) => assert!(
                matches!(finding.provenance, Provenance::Anchored { marker_dir, .. } if marker_dir == fixture.path())
            ),
            other => panic!("expected an artifact verdict, got {other:?}"),
        }
    }

    /// The data-loss regression test. A `target/` with nothing proving it is somebody's data.
    #[test]
    fn should_not_classify_target_without_a_marker() {
        let fixture = tree(&["target/", "target/important.txt"]);
        let classifier = classifier();
        match verdict(&classifier, fixture.path(), "target") {
            Verdict::Skip(SkipReason::NoMarker { ecosystem, .. }) => {
                assert!(
                    ["rust", "maven", "scala"].contains(&ecosystem),
                    "unexpected ecosystem {ecosystem}"
                );
            }
            other => panic!("expected a no-marker skip, got {other:?}"),
        }
    }

    #[test]
    fn should_resolve_an_ambiguous_name_by_ecosystem_not_precedence() {
        let rust = tree(&["Cargo.toml", "target/"]);
        let maven = tree(&["pom.xml", "target/"]);
        let classifier = classifier();
        assert_artifact(&verdict(&classifier, rust.path(), "target"), "rust", "target/");
        assert_artifact(&verdict(&classifier, maven.path(), "target"), "maven", "target/");
    }

    #[test]
    fn should_report_a_missing_marker_rather_than_a_lesser_reason() {
        let fixture = tree(&["bin/"]);
        let classifier = classifier();
        // `bin/` is Go's (off by default) and .NET's (on). The .NET no-marker explanation is
        // more useful than Go's opt-in hint, so it wins.
        match verdict(&classifier, fixture.path(), "bin") {
            Verdict::Skip(SkipReason::NoMarker { ecosystem, .. }) => assert_eq!(ecosystem, "dotnet"),
            other => panic!("expected a no-marker skip, got {other:?}"),
        }
    }

    #[test]
    fn should_prove_dotnet_obj_through_an_ancestor_marker() {
        for depth in 0..=3 {
            let nesting = "src/".repeat(depth);
            let fixture = tree(&["App.csproj", &format!("{nesting}obj/")]);
            let classifier = classifier();
            let verdict = verdict(&classifier, fixture.path(), &format!("{nesting}obj"));
            assert_artifact(&verdict, "dotnet", "obj/");
        }
    }

    #[test]
    fn should_not_prove_dotnet_obj_beyond_the_ancestor_bound() {
        let fixture = tree(&["App.csproj", "a/b/c/d/obj/"]);
        let classifier = classifier();
        match verdict(&classifier, fixture.path(), "a/b/c/d/obj") {
            Verdict::Skip(SkipReason::NoMarker { ecosystem, .. }) => assert_eq!(ecosystem, "dotnet"),
            other => panic!("expected a no-marker skip four levels down, got {other:?}"),
        }
    }

    /// A marker outside the scanned tree must not justify a deletion inside it.
    #[test]
    fn should_not_climb_above_the_scan_root_for_a_marker() {
        let fixture = tree(&["App.csproj", "project/nested/obj/"]);
        let classifier = classifier();
        let root = fixture.path().join("project");
        let path = root.join("nested/obj");
        match classifier.classify(&root, &path, true) {
            Verdict::Skip(SkipReason::NoMarker { .. }) => {}
            other => panic!("expected the climb to stop at the scan root, got {other:?}"),
        }
    }

    #[test]
    fn should_prove_pycache_one_level_below_the_project_root() {
        let fixture = tree(&["pyproject.toml", "package/__pycache__/"]);
        let classifier = classifier();
        assert_artifact(
            &verdict(&classifier, fixture.path(), "package/__pycache__"),
            "python",
            "__pycache__/",
        );
    }

    #[test]
    fn should_verify_leading_segments_of_a_multi_segment_artifact() {
        let matching = tree(&["Gemfile", "vendor/bundle/"]);
        let classifier = classifier();
        assert_artifact(
            &verdict(&classifier, matching.path(), "vendor/bundle"),
            "ruby",
            "vendor/bundle/",
        );

        // The same trailing name in the wrong place is not the artifact.
        let elsewhere = tree(&["Gemfile", "lib/bundle/"]);
        assert_eq!(
            verdict(&classifier, elsewhere.path(), "lib/bundle"),
            Verdict::NotACandidate
        );
    }

    #[test]
    fn should_require_a_directory_for_a_slash_terminated_artifact() {
        let fixture = tree(&["Cargo.toml", "target"]);
        let classifier = classifier();
        assert_eq!(verdict(&classifier, fixture.path(), "target"), Verdict::NotACandidate);
    }

    #[test]
    fn should_match_a_file_artifact_that_is_not_slash_terminated() {
        let fixture = tree(&["package.json", "app.tsbuildinfo"]);
        let classifier = classifier();
        assert_artifact(
            &verdict(&classifier, fixture.path(), "app.tsbuildinfo"),
            "node",
            "*.tsbuildinfo",
        );
    }

    #[test]
    fn should_match_a_prefix_glob_artifact() {
        let fixture = tree(&["CMakeLists.txt", "cmake-build-debug/"]);
        let classifier = classifier();
        assert_artifact(
            &verdict(&classifier, fixture.path(), "cmake-build-debug"),
            "cmake",
            "cmake-build-*/",
        );
    }

    #[test]
    fn should_skip_an_off_by_default_artifact_and_say_how_to_enable_it() {
        let fixture = tree(&["go.mod", "bin/"]);
        let classifier = classifier();
        match verdict(&classifier, fixture.path(), "bin") {
            Verdict::Skip(SkipReason::NotEnabled { spec, note }) => {
                assert_eq!(spec, "go.bin");
                assert!(note.is_some(), "an opt-in artifact must explain itself");
            }
            other => panic!("expected an opt-in skip, got {other:?}"),
        }
    }

    #[test]
    fn should_remove_an_off_by_default_artifact_once_enabled() {
        let fixture = tree(&["go.mod", "bin/"]);
        let mut selection = Selection::all();
        assert!(selection.set("go.bin", true));
        let classifier = Classifier::new(selection).unwrap();
        assert_artifact(&verdict(&classifier, fixture.path(), "bin"), "go", "bin/");
    }

    #[test]
    fn should_not_classify_when_the_ecosystem_is_deselected() {
        let fixture = tree(&["Cargo.toml", "target/"]);
        let (selection, unknown) = Selection::only(["node"]);
        assert!(unknown.is_empty());
        let classifier = Classifier::new(selection).unwrap();
        match verdict(&classifier, fixture.path(), "target") {
            Verdict::Skip(SkipReason::NotEnabled { .. }) => {}
            other => panic!("expected a deselected skip, got {other:?}"),
        }
    }

    #[test]
    fn should_report_an_unknown_ecosystem_rather_than_ignoring_it() {
        let (_, unknown) = Selection::only(["rust", "rustlang"]);
        assert_eq!(unknown, vec!["rustlang".to_owned()]);
    }

    /// Memoization is the reason ADR 0002's safety costs almost nothing. Twenty siblings that
    /// all anchor on the same directory must list it once, not twenty times.
    #[test]
    fn should_list_each_anchor_directory_exactly_once() {
        let mut entries = vec!["Cargo.toml".to_owned()];
        for index in 0..20 {
            entries.push(format!("crate{index}/Cargo.toml"));
            entries.push(format!("crate{index}/target/"));
        }
        let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
        let fixture = tree(&refs);
        let classifier = classifier();

        for index in 0..20 {
            assert_artifact(
                &verdict(&classifier, fixture.path(), &format!("crate{index}/target")),
                "rust",
                "target/",
            );
        }
        // One probe per crate directory, and none for the twenty `target/` directories
        // themselves — those are never anchors.
        assert_eq!(classifier.probe_count(), 20);
    }

    #[test]
    fn should_leave_an_unreadable_anchor_unproven() {
        let fixture = tree(&["Cargo.toml", "target/"]);
        let classifier = classifier();
        let missing = fixture.path().join("gone");
        // Classifying against an anchor that does not exist proves nothing, which means the
        // candidate survives — the safe direction when voom cannot tell.
        match classifier.classify(fixture.path(), &missing.join("target"), true) {
            Verdict::Skip(SkipReason::Unreadable) => {}
            other => panic!("expected an unreadable skip, got {other:?}"),
        }
    }

    #[test]
    fn should_treat_an_unrelated_name_as_not_a_candidate() {
        let fixture = tree(&["Cargo.toml", "src/"]);
        let classifier = classifier();
        assert_eq!(verdict(&classifier, fixture.path(), "src"), Verdict::NotACandidate);
        assert_eq!(
            classifier.probe_count(),
            0,
            "a non-candidate must not cost a directory listing"
        );
    }

    #[test]
    fn should_recognise_dependency_directories() {
        assert!(is_dependency_dir(OsStr::new("node_modules")));
        assert!(is_dependency_dir(OsStr::new("vendor")));
        assert!(!is_dependency_dir(OsStr::new("target")));
    }
}
