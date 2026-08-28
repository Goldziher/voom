//! The claims this project makes about itself, checked against the project.
//!
//! `.ai-rulez/context/code-style.md` requires that documentation making a claim about behaviour
//! matches the code, and names this table specifically. Without a test that is an aspiration:
//! the table is hand-maintained markdown and the catalog is compiled-in data, and nothing
//! otherwise stops them drifting apart the first time an ecosystem is added.

// Helper functions here are not `#[test]` bodies, which is all `allow-expect-in-tests` covers.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::BTreeSet;
use std::fs;

use tempfile::TempDir;
use voom::catalog::CATALOG;
use voom::run::{RunOptions, run};

/// The first column of the README's "Supported ecosystems" table.
fn readme_ecosystems() -> Vec<String> {
    let readme = include_str!("../README.md");
    readme
        .lines()
        .skip_while(|line| !line.starts_with("| Ecosystem | Marker | Artifacts |"))
        .skip(2)
        .take_while(|line| line.starts_with("| "))
        .filter_map(|line| line.split('|').nth(1))
        .map(|cell| cell.trim().to_owned())
        .collect()
}

#[test]
fn the_readme_table_lists_exactly_the_catalog() {
    let documented: BTreeSet<String> = readme_ecosystems().into_iter().collect();
    let shipped: BTreeSet<String> = CATALOG.iter().map(|ecosystem| ecosystem.name.to_owned()).collect();

    assert!(
        !documented.is_empty(),
        "the README's ecosystem table could not be found"
    );

    let missing: Vec<_> = shipped.difference(&documented).collect();
    assert!(
        missing.is_empty(),
        "in the catalog but not in the README's table: {missing:?}"
    );

    let extra: Vec<_> = documented.difference(&shipped).collect();
    assert!(
        extra.is_empty(),
        "in the README's table but not in the catalog: {extra:?}"
    );
}

#[test]
fn the_readme_table_has_one_row_per_ecosystem() {
    assert_eq!(
        readme_ecosystems().len(),
        CATALOG.len(),
        "the README's ecosystem table and the catalog disagree on how many there are"
    );
}

/// The other direction: every marker and artifact the catalog ships must appear in the row
/// that claims to describe it.
///
/// The tests around this one all check that what the README says is true. None of them
/// checked that what the README leaves out is nothing — so six rows were quietly missing
/// markers and artifacts they matched on, and a reader deciding whether `bin/` in their .NET
/// project was at risk could not have found out from the table.
#[test]
fn the_readme_table_omits_nothing_the_catalog_ships() {
    let readme = include_str!("../README.md");
    let rows: Vec<&str> = readme
        .lines()
        .skip_while(|line| !line.starts_with("| Ecosystem | Marker | Artifacts |"))
        .skip(2)
        .take_while(|line| line.starts_with("| "))
        .collect();
    assert_eq!(rows.len(), CATALOG.len(), "one row per ecosystem");

    let mut omissions = Vec::new();
    for ecosystem in CATALOG {
        let row = rows
            .iter()
            .find(|row| row.split('|').nth(1).is_some_and(|cell| cell.trim() == ecosystem.name))
            .unwrap_or_else(|| panic!("no README row for `{}`", ecosystem.name));

        // The README's marker cell, as the tokens a reader sees.
        let advertised: Vec<&str> = row
            .split('|')
            .nth(2)
            .unwrap_or_default()
            .split(',')
            .map(|token| token.trim().trim_matches('`'))
            .filter(|token| !token.is_empty())
            .collect();

        for marker in ecosystem.markers {
            // A README token may end in `*` to stand for a family — `settings.gradle*` covers
            // both the Groovy and the Kotlin spelling. It stands for nothing else: matching on
            // a bare `*` anywhere in the cell would let `*.csproj` vouch for `*.fsproj`.
            let named = advertised
                .iter()
                .any(|token| *token == *marker || token.strip_suffix('*').is_some_and(|stem| marker.starts_with(stem)));
            if !named {
                omissions.push(format!("{}: marker `{marker}`", ecosystem.name));
            }
        }

        for artifact in ecosystem.artifacts {
            if !row.contains(artifact.path) {
                omissions.push(format!("{}: artifact `{}`", ecosystem.name, artifact.path));
            }
        }
    }

    assert!(
        omissions.is_empty(),
        "the catalog matches on these, and the README's table does not say so:\n  {}",
        omissions.join("\n  ")
    );
}

/// Every marker the README advertises must really prove its ecosystem, so a reader deciding
/// whether to trust voom against `$HOME` is reading the truth.
#[test]
fn every_marker_the_readme_advertises_is_a_real_marker() {
    let readme = include_str!("../README.md");
    let rows: Vec<&str> = readme
        .lines()
        .skip_while(|line| !line.starts_with("| Ecosystem | Marker | Artifacts |"))
        .skip(2)
        .take_while(|line| line.starts_with("| "))
        .collect();

    for row in rows {
        let mut cells = row.split('|').skip(1);
        let name = cells.next().unwrap_or_default().trim();
        let markers = cells.next().unwrap_or_default();
        let ecosystem = CATALOG
            .iter()
            .find(|ecosystem| ecosystem.name == name)
            .unwrap_or_else(|| panic!("`{name}` is in the README but not the catalog"));

        for documented in markers.split(',') {
            let documented = documented.trim().trim_matches('`');
            if documented.is_empty() {
                continue;
            }
            assert!(
                ecosystem.markers.contains(&documented),
                "`{name}`: the README advertises the marker `{documented}`, which the catalog \
                 does not declare (it has {:?})",
                ecosystem.markers
            );
        }
    }
}

/// Every `voom.toml` example in the README, parsed by the real loader.
///
/// The blocks are identified by what they are *not*: the git-hooks section carries a
/// `[[hooks.sources]]` example, which is poly's schema rather than voom's.
fn readme_config_examples() -> Vec<String> {
    let readme = include_str!("../README.md");
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in readme.lines() {
        match (&mut current, line.trim()) {
            (None, "```toml") => current = Some(String::new()),
            (Some(_), "```") => blocks.push(current.take().unwrap_or_default()),
            (Some(block), _) => {
                block.push_str(line);
                block.push('\n');
            }
            (None, _) => {}
        }
    }
    blocks.retain(|block| !block.contains("[[hooks."));
    blocks
}

/// A configuration example nobody can use is worse than none: every struct in the schema is
/// `deny_unknown_fields`, so one stale key turns a copied example into a hard load error. TOML
/// ordering is the other trap — a top-level key written after a `[table]` silently becomes a
/// key *of* that table, which is how `exclude` once stopped being an exclusion.
///
/// Run through the real pipeline rather than against the schema directly, because that is what
/// a reader copying the block actually does.
#[test]
fn every_readme_configuration_example_loads() {
    let examples = readme_config_examples();
    assert!(!examples.is_empty(), "the README documents configuration somewhere");

    for example in examples {
        let fixture = TempDir::new().expect("a temp dir");
        fs::write(fixture.path().join("voom.toml"), &example).expect("a config file");

        let options = RunOptions {
            dry_run: true,
            ..support::options(fixture.path())
        };
        if let Err(error) = run(&options) {
            panic!("a README example does not load: {error}\n\n{example}");
        }
    }
}

/// The keys the prose promises are the keys the schema accepts. `deny_unknown_fields` turns a
/// typo into a hard error, so a key named in the README that voom does not know is a config
/// file that fails for everyone who copies it.
#[test]
fn every_top_level_key_the_readme_documents_is_accepted() {
    for key in ["exclude", "include"] {
        let fixture = TempDir::new().expect("a temp dir");
        fs::write(fixture.path().join("voom.toml"), format!("{key} = []\n")).expect("a config file");

        let options = RunOptions {
            dry_run: true,
            ..support::options(fixture.path())
        };
        assert!(run(&options).is_ok(), "`{key}` is documented but not accepted");
    }
}

/// Every `.rs` file under `src/`, with its line count.
fn source_files(dir: &std::path::Path, found: &mut Vec<(std::path::PathBuf, usize)>) {
    let entries = fs::read_dir(dir).expect("src/ is readable");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let lines = fs::read_to_string(&path).expect("a source file").lines().count();
            found.push((path, lines));
        }
    }
}

/// The 1000-line module cap, which `module-size-cap` calls critical.
///
/// The rule claimed poly enforced it at commit time. No such hook exists, in `poly.toml` or in
/// CI, so until now the cap was honour-system — and the file nearest it has about twenty lines
/// of headroom, which is one careless addition.
///
/// The cap is not about tidiness. voom's correctness argument (ADR 0002) rests on a classifier
/// a reviewer can hold in their head; a 2000-line `classify.rs` is one where a wrong anchor
/// hides, and a wrong anchor deletes someone's source directory.
#[test]
fn no_source_file_exceeds_the_module_size_cap() {
    const CAP: usize = 1000;

    let mut files = Vec::new();
    source_files(std::path::Path::new("src"), &mut files);
    assert!(files.len() > 10, "the walk found almost nothing — is the path right?");

    let over: Vec<String> = files
        .iter()
        .filter(|(_, lines)| *lines > CAP)
        .map(|(path, lines)| format!("{} is {lines} lines", path.display()))
        .collect();

    assert!(
        over.is_empty(),
        "over the {CAP}-line cap:\n  {}\n\nSplit along a seam that already exists — the catalog \
         splits by ecosystem group, the reporter by output format, config by load/merge/resolve. \
         If no seam presents itself, that is the signal the module is doing two things.",
        over.join("\n  ")
    );
}
