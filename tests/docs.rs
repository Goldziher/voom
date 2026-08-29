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

/// Every `voom.toml` example in the documentation, parsed by the real loader.
///
/// Reads the whole set, not just the README: most of the schema lives in
/// `docs/configuration.md` now, and an example that stopped being checked the moment it moved
/// would be the worst possible outcome of tidying the front page.
///
/// The blocks are identified by what they are *not*: the git-hooks section carries a
/// `[[hooks.sources]]` example, which is poly's schema rather than voom's.
fn readme_config_examples() -> Vec<String> {
    let readme = documentation();
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

/// Does this line write out a literal number of the ecosystems voom supports?
///
/// Matches a run of digits sitting immediately before `ecosystem`/`ecosystems`, optionally
/// with `language` between them. That covers every shape the count was ever written in: the
/// one-line description's `across 24 language ecosystems`, the README feature list's
/// `**24 ecosystems**`, and its opening paragraph's `the rest of 24 ecosystems`.
///
/// `holding N ecosystems` is the one exception, and it is not a support claim — it counts what
/// was in the benchmark corpus the README's timings were measured against, which is a fact
/// about that machine and would be wrong to erase.
fn states_an_ecosystem_count(line: &str) -> bool {
    line.match_indices("ecosystem").any(|(index, _)| {
        let before = line[..index].trim_end();
        let before = before.strip_suffix("language").map_or(before, str::trim_end);
        let subject = before.trim_end_matches(|character: char| character.is_ascii_digit());
        subject.len() != before.len() && !subject.trim_end().ends_with("holding")
    })
}

/// Every live surface in the repository that writes the ecosystem count out by hand.
///
/// Returns the offending `(path, line)` pairs and the number of text files read, so a caller
/// can tell "nothing is broken" apart from "the walk found nothing to look at".
fn hard_coded_ecosystem_counts() -> (Vec<(std::path::PathBuf, String)>, usize) {
    const SKIP: [&str; 4] = ["target", ".git", "node_modules", "__pycache__"];
    // Dated records, not live surfaces: `CHANGELOG.md` and the ADRs describe what was true at a
    // point in time and would be wrong to update. This file is exempt because a test forbidding
    // a phrase has to be able to quote it.
    const EXEMPT: [&str; 3] = ["./CHANGELOG.md", "./adrs", "./tests/docs.rs"];

    fn walk(dir: &std::path::Path, found: &mut Vec<(std::path::PathBuf, String)>, scanned: &mut usize) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if SKIP.iter().any(|skip| name == *skip) || EXEMPT.iter().any(|exempt| path == std::path::Path::new(exempt))
            {
                continue;
            }
            if path.is_dir() {
                walk(&path, found, scanned);
                continue;
            }
            // Binary files (the banner PNGs, the compiled Python) simply do not read as UTF-8.
            let Ok(text) = fs::read_to_string(&path) else { continue };
            *scanned += 1;
            for line in text.lines() {
                if states_an_ecosystem_count(line) {
                    found.push((path.clone(), line.trim().to_owned()));
                }
            }
        }
    }

    let mut found = Vec::new();
    let mut scanned = 0;
    walk(std::path::Path::new("."), &mut found, &mut scanned);
    (found, scanned)
}

/// No surface writes the ecosystem count out by hand.
///
/// This replaces `every_stated_ecosystem_count_matches_the_catalog`, which asserted that the
/// ten hand-written copies of the number agreed with `CATALOG`. That treated the symptom and
/// kept the disease: adding an ecosystem still meant editing ten files, and all the test did
/// was fail the build until you had. Forbidding the count outright is the stronger invariant —
/// a number that is never written cannot go stale, and nobody has to remember to bump it.
///
/// Two surfaces still state how many there are, and neither can drift: `voom catalog` prints
/// `CATALOG` directly, and the README's ecosystem table is checked against `CATALOG` in both
/// directions by the tests above. Everything else — the crate, npm and `PyPI` manifests, the
/// two package READMEs, the `--help` text, the generated Homebrew formula, `.ai-rulez/config.toml`
/// and the agent files generated from it — carries prose with no number in it.
///
/// Discovery is by walking the tree rather than by a fixed list, so a surface added later is
/// covered without anyone remembering to add it here.
#[test]
fn no_surface_hard_codes_an_ecosystem_count() {
    assert!(
        states_an_ecosystem_count("build-artifact pruning across 24 language ecosystems"),
        "the matcher no longer recognises the phrasing it exists to forbid"
    );
    assert!(
        states_an_ecosystem_count("- **24 ecosystems** — Rust, Node, Python"),
        "the matcher no longer recognises a bare count before the noun"
    );
    assert!(
        !states_an_ecosystem_count("build-artifact pruning across every major language ecosystem"),
        "the matcher flags countless prose, so it would fail no matter what anyone wrote"
    );
    assert!(
        !states_an_ecosystem_count("a 130 GB `~/workspace` holding 16 ecosystems, and a `$HOME` above it"),
        "the matcher flags the benchmark corpus, which counts a fixture tree and not what voom supports"
    );

    let (found, scanned) = hard_coded_ecosystem_counts();
    assert!(
        scanned > 50,
        "the walk read only {scanned} text files — is the path right?"
    );

    let stated: Vec<String> = found
        .iter()
        .map(|(path, line)| format!("{}: {line}", path.display()))
        .collect();

    assert!(
        stated.is_empty(),
        "the number of ecosystems is written out by hand here:\n  {}\n\nSay it without a count. \
         `voom catalog` prints the catalog and the README's table is checked against it, so those \
         two are the only places the number belongs.",
        stated.join("\n  ")
    );
}

/// The documentation set: the README plus everything under `docs/`.
///
/// Reference material lives in `docs/` so the README can stay a README, but "documented" means
/// documented *somewhere a reader is pointed at* — checking only the front page would have made
/// moving a table into `docs/` look like deleting it.
fn documentation() -> String {
    let mut all = include_str!("../README.md").to_owned();
    all.push_str(include_str!("../docs/reference.md"));
    all.push_str(include_str!("../docs/configuration.md"));
    all
}

/// Every long flag the CLI accepts must appear in the documentation.
///
/// The README drifted a full version silently: `--summary`, `--color`, `--disable`, `--config`,
/// `--exclude` as a flag and `git-prune --timeout` were all undocumented while the ecosystem
/// table — the one thing a test checked — stayed perfect. A reference that is only correct when
/// somebody remembers to update it is not a reference.
///
/// Introspects clap rather than parsing `cli.rs`, so a flag cannot hide behind an attribute this
/// test did not anticipate.
#[test]
fn every_flag_the_cli_accepts_is_documented() {
    use clap::CommandFactory;

    let readme = documentation();
    let command = voom::cli::Cli::command();
    let mut missing = Vec::new();

    collect_undocumented_flags(&command, &readme, &mut missing);

    assert!(
        missing.is_empty(),
        "the CLI accepts these and nothing in the README or docs/ mentions them:\n  {}",
        missing.join("\n  ")
    );
}

/// Every subcommand must appear too — they are the surface a reader is most likely to miss,
/// because `--help` groups flags but buries commands.
#[test]
fn every_subcommand_is_documented() {
    use clap::CommandFactory;

    let readme = documentation();
    let missing: Vec<String> = voom::cli::Cli::command()
        .get_subcommands()
        .map(clap::Command::get_name)
        .filter(|name| !readme.contains(&format!("voom {name}")))
        .map(str::to_owned)
        .collect();

    assert!(missing.is_empty(), "subcommands absent from the README: {missing:?}");
}

/// Walks a command and its subcommands, collecting long flags the README does not mention.
fn collect_undocumented_flags(command: &clap::Command, readme: &str, missing: &mut Vec<String>) {
    let scope = command.get_name().to_owned();
    for argument in command.get_arguments() {
        // `--help` and `--version` are clap's own and universally understood; documenting them
        // would be noise, not completeness.
        if matches!(argument.get_id().as_str(), "help" | "version") {
            continue;
        }
        let Some(long) = argument.get_long() else {
            continue;
        };
        if !readme.contains(&format!("--{long}")) {
            missing.push(format!("{scope}: --{long}"));
        }
    }
    for subcommand in command.get_subcommands() {
        collect_undocumented_flags(subcommand, readme, missing);
    }
}

/// Every cache id must be documented, because the id is what `--clean-caches` takes and it is not
/// always the tool's name — a reader who guesses `gradle` instead of `gradle-caches` gets an
/// error, and prose naming the tools does not tell them that.
#[test]
fn every_cache_id_is_documented() {
    let readme = documentation();
    let missing: Vec<&str> = voom::caches::CACHES
        .iter()
        .map(|cache| cache.id)
        .filter(|id| !readme.contains(*id))
        .collect();

    assert!(missing.is_empty(), "cache ids absent from the README: {missing:?}");
}
