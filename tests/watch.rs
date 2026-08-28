//! Watch mode's quiet-period rail, per ecosystem.
//!
//! ADR 0008 is Proposed until fixtures for at least Rust, Node and Gradle demonstrate that an
//! in-progress build is never deleted. These are those fixtures.
//!
//! They drive [`QuietTracker`] with simulated timestamps rather than real sleeps and a real
//! watcher: the rule under test is "does continuous activity keep a subtree from ever looking
//! idle", which is a property of the tracker. A wall-clock test of the same thing would be slow
//! and flaky, and would test the debouncer's timing rather than the safety rule.

// Helper functions here are not `#[test]` bodies, which is all `allow-expect-in-tests` covers.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use support::snapshot;
use tempfile::TempDir;
use voom::config::resolve::Flags;
use voom::run::{RunOptions, run};
use voom::watch::QuietTracker;

/// The build a given ecosystem performs: its marker, its artifact, and the paths a compiler
/// touches while it runs.
struct Build {
    ecosystem: &'static str,
    marker: &'static str,
    artifact: &'static str,
    written: &'static [&'static str],
}

const BUILDS: &[Build] = &[
    Build {
        ecosystem: "rust",
        marker: "Cargo.toml",
        artifact: "target",
        written: &[
            "target/debug/incremental/app-1a2b/s-h1/query-cache.bin",
            "target/debug/deps/libapp.rlib",
            "target/debug/app",
        ],
    },
    Build {
        ecosystem: "node",
        marker: "package.json",
        artifact: "dist",
        written: &["dist/index.js", "dist/chunks/vendor.js", "dist/index.js.map"],
    },
    Build {
        ecosystem: "gradle",
        marker: "build.gradle.kts",
        artifact: "build",
        written: &[
            "build/classes/kotlin/main/App.class",
            "build/tmp/compileKotlin/output.txt",
            "build/libs/app.jar",
        ],
    },
];

fn project(build: &Build) -> TempDir {
    let root = TempDir::new().expect("a temporary directory");
    fs::write(root.path().join(build.marker), b"marker").unwrap();
    for written in build.written {
        let path = root.path().join(written);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"build output").unwrap();
    }
    root
}

fn options(root: &Path) -> RunOptions {
    RunOptions {
        roots: vec![root.to_path_buf()],
        dry_run: true,
        jobs: Some(2),
        one_file_system: true,
        verbose: false,
        config: None,
        progress: false,
        flags: Flags {
            enable: vec!["gradle.build".to_owned()],
            ..Flags::default()
        },
    }
}

/// The central claim: a build that keeps writing is never seen as idle, for any ecosystem.
#[test]
fn an_in_progress_build_is_never_quiet() {
    for build in BUILDS {
        let root = project(build);
        let artifact = root.path().join(build.artifact);
        let base = Instant::now();
        let quiet_period = Duration::from_secs(60);
        let mut tracker = QuietTracker::new();

        // Ten minutes of a compiler writing into its output directory, one file per second.
        for second in 0..600usize {
            let written = build.written[second % build.written.len()];
            let at = base + Duration::from_secs(second as u64);
            tracker.touch(&root.path().join(written), at);

            assert!(
                !tracker.is_quiet(&artifact, quiet_period, at),
                "{}: `{}` looked idle {second}s into an active build — watch mode would have \
                 deleted it out from under the compiler",
                build.ecosystem,
                build.artifact
            );
        }

        // The build finishes. The artifact is released only after the full quiet period.
        let finished = base + Duration::from_secs(599);
        assert!(!tracker.is_quiet(&artifact, quiet_period, finished + Duration::from_secs(59)));
        assert!(tracker.is_quiet(&artifact, quiet_period, finished + Duration::from_secs(60)));
    }
}

/// A pause inside a build — a slow test suite between compile phases — is the known weakness
/// (ADR 0008). This pins the actual behaviour so a change to it is deliberate: a pause shorter
/// than the quiet period is survived, and the default is 60s precisely to make that likely.
#[test]
fn a_pause_shorter_than_the_quiet_period_does_not_release_the_build() {
    let build = &BUILDS[0];
    let root = project(build);
    let artifact = root.path().join(build.artifact);
    let base = Instant::now();
    let quiet_period = Duration::from_secs(60);

    let mut tracker = QuietTracker::new();
    tracker.touch(&root.path().join(build.written[0]), base);

    // A 45-second test run between compile phases.
    assert!(!tracker.is_quiet(&artifact, quiet_period, base + Duration::from_secs(45)));

    tracker.touch(&root.path().join(build.written[1]), base + Duration::from_secs(45));
    assert!(!tracker.is_quiet(&artifact, quiet_period, base + Duration::from_secs(100)));
}

/// Each ecosystem's artifact really is reclaimable once quiet — otherwise the tests above would
/// pass by watching something voom never touches.
#[test]
fn each_build_produces_something_voom_would_reclaim_when_quiet() {
    for build in BUILDS {
        let root = project(build);
        let before = snapshot(root.path());

        let result = run(&options(root.path())).expect("the run completes");

        let paths: Vec<PathBuf> = result.entries.iter().map(|entry| entry.path.clone()).collect();
        assert!(
            paths.contains(&root.path().join(build.artifact)),
            "{}: `{}` was not recognised as reclaimable, so the quiet-period test proves nothing",
            build.ecosystem,
            build.artifact
        );
        assert_eq!(snapshot(root.path()), before, "the dry run must not touch the tree");
    }
}

/// Activity anywhere below the artifact counts, however deep. A compiler writing into
/// `target/debug/incremental/…` must hold `target/` itself.
#[test]
fn activity_at_any_depth_holds_the_artifact() {
    for build in BUILDS {
        let root = project(build);
        let artifact = root.path().join(build.artifact);
        let base = Instant::now();
        let deepest = build
            .written
            .iter()
            .max_by_key(|path| path.matches('/').count())
            .expect("a path");

        let mut tracker = QuietTracker::new();
        tracker.touch(&root.path().join(deepest), base);

        assert!(
            !tracker.is_quiet(&artifact, Duration::from_secs(60), base + Duration::from_secs(1)),
            "{}: writing `{deepest}` did not hold `{}`",
            build.ecosystem,
            build.artifact
        );
    }
}
