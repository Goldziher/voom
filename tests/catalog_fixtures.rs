//! Every catalog entry, proved and disproved.
//!
//! CONTRIBUTING requires two fixtures per entry: one where the marker is present and the
//! artifact is removed, and one where the artifact exists *without* its marker and is left
//! alone. The second is the one that matters — it is the regression test for data loss.
//!
//! These suites iterate [`CATALOG`] rather than naming entries, so an ecosystem added tomorrow
//! is covered the moment it lands and cannot be merged without its negative case.

// Helper functions here are not `#[test]` bodies, which is all `allow-expect-in-tests` covers.
// In a test crate a panic is the failure report, which is exactly what these want.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::Path;

use support::{describe, fixture, snapshot};
use voom::catalog::CATALOG;
use voom::config::resolve::Flags;
use voom::run::{RunOptions, run};

fn options(root: &Path, enable: Vec<String>) -> RunOptions {
    RunOptions {
        roots: vec![root.to_path_buf()],
        dry_run: false,
        jobs: Some(2),
        one_file_system: true,
        caches: false,
        verbose: true,
        config: None,
        progress: false,
        flags: Flags {
            enable,
            ..Flags::default()
        },
    }
}

/// Positive: the marker proves the ecosystem, so the artifact goes.
#[test]
fn every_entry_removes_its_artifact_when_a_marker_proves_it() {
    for ecosystem in CATALOG {
        for artifact in ecosystem.artifacts {
            let (root, target) = fixture(ecosystem, artifact, true);
            let enable = if artifact.default_on {
                Vec::new()
            } else {
                vec![format!("{}.{}", ecosystem.id, artifact.slug())]
            };

            let result = run(&options(root.path(), enable)).expect("the run completes");

            assert!(
                !target.exists(),
                "{}: proven by `{}` but not removed. Report: {:?}",
                describe(ecosystem, artifact),
                ecosystem.markers[0],
                result
                    .entries
                    .iter()
                    .map(|entry| (&entry.path, &entry.outcome))
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// Negative: no marker, no deletion. **This is the data-loss regression test.**
///
/// Every artifact directory in the catalog is also a plausible hand-written directory in some
/// tree. If any of these ever disappears, voom is deleting on a name alone and ADR 0002 has
/// been broken. Fix the classifier, never this test.
#[test]
fn every_entry_leaves_its_artifact_alone_without_a_marker() {
    for ecosystem in CATALOG {
        for artifact in ecosystem.artifacts {
            let (root, target) = fixture(ecosystem, artifact, false);
            let before = snapshot(root.path());

            // Everything enabled, so the only thing standing between the artifact and deletion
            // is the missing marker.
            let enable = vec![format!("{}.{}", ecosystem.id, artifact.slug())];
            let result = run(&options(root.path(), enable)).expect("the run completes");

            assert!(
                target.exists(),
                "{}: removed with no marker present — this is data loss",
                describe(ecosystem, artifact)
            );
            assert_eq!(
                snapshot(root.path()),
                before,
                "{}: the tree changed with no marker present",
                describe(ecosystem, artifact)
            );
            assert!(
                result.entries.is_empty(),
                "{}: reported an artifact with nothing proving it",
                describe(ecosystem, artifact)
            );
        }
    }
}

/// An off-by-default artifact stays until it is asked for, even with its marker present.
#[test]
fn every_opt_in_artifact_survives_until_it_is_enabled() {
    let mut checked = 0;
    for ecosystem in CATALOG {
        for artifact in ecosystem.artifacts.iter().filter(|artifact| !artifact.default_on) {
            checked += 1;
            let (root, target) = fixture(ecosystem, artifact, true);

            run(&options(root.path(), Vec::new())).expect("the run completes");
            assert!(
                target.exists(),
                "{}: removed without the opt-in it declares",
                describe(ecosystem, artifact)
            );

            let enable = vec![format!("{}.{}", ecosystem.id, artifact.slug())];
            run(&options(root.path(), enable)).expect("the run completes");
            assert!(
                !target.exists(),
                "{}: still there after being enabled explicitly",
                describe(ecosystem, artifact)
            );
        }
    }
    assert!(
        checked > 0,
        "the catalog has opt-in artifacts and they must be exercised"
    );
}

/// A dry run over every entry leaves every tree byte-identical, and reports exactly what a real
/// run then removes.
#[test]
fn a_dry_run_over_every_entry_changes_nothing_and_predicts_the_real_run() {
    for ecosystem in CATALOG {
        for artifact in ecosystem.artifacts {
            let (root, _) = fixture(ecosystem, artifact, true);
            let enable = vec![format!("{}.{}", ecosystem.id, artifact.slug())];
            let before = snapshot(root.path());

            let dry = run(&RunOptions {
                dry_run: true,
                ..options(root.path(), enable.clone())
            })
            .expect("the dry run completes");
            assert_eq!(
                snapshot(root.path()),
                before,
                "{}: a dry run modified the tree",
                describe(ecosystem, artifact)
            );

            let real = run(&options(root.path(), enable)).expect("the run completes");
            let predicted: Vec<_> = dry.entries.iter().map(|entry| entry.path.clone()).collect();
            let actual: Vec<_> = real.entries.iter().map(|entry| entry.path.clone()).collect();
            assert_eq!(
                predicted,
                actual,
                "{}: the dry run and the real run disagree",
                describe(ecosystem, artifact)
            );
        }
    }
}
