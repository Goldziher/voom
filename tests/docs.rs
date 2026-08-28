//! The README's ecosystem table against the catalog it describes.
//!
//! `.ai-rulez/context/code-style.md` requires that documentation making a claim about behaviour
//! matches the code, and names this table specifically. Without a test that is an aspiration:
//! the table is hand-maintained markdown and the catalog is compiled-in data, and nothing
//! otherwise stops them drifting apart the first time an ecosystem is added.

// Helper functions here are not `#[test]` bodies, which is all `allow-expect-in-tests` covers.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use voom::catalog::CATALOG;

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
