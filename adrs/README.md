# Architecture Decision Records

These ADRs are the source of truth for how voom is designed and why. Code, configuration,
and documentation follow them; where an implementation and an ADR disagree, one of the two is
a bug. Read 0001 first — it sets the scope every other decision operates inside — then 0002,
which is the safety rule the rest of the tool is built around.

## Index

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-mission-and-scope.md) | Mission and Scope: Fast, Safe, Recursive Build-Artifact Pruning | Accepted |
| [0002](0002-marker-anchored-classification.md) | Marker-Anchored Classification: Never Delete on Directory Name Alone | Accepted |
| [0003](0003-builtin-catalog.md) | Built-in Catalog: Data-Driven Ecosystem Definitions | Accepted |
| [0004](0004-configuration.md) | Configuration: Hierarchical TOML, Include/Exclude, Keep Policies | Accepted |
| [0005](0005-traversal-and-parallelism.md) | Traversal and Parallelism: Ignore-Blind Walking Over rayon | Accepted |
| [0006](0006-deletion-semantics.md) | Deletion Semantics: Default-Delete, Dry-Run, and the Safety Rails | Accepted |
| [0007](0007-reporting.md) | Reporting: Colored, Deterministic, Machine-Readable | Accepted |
| [0008](0008-watch-mode.md) | Watch Mode: Debounced `notify` Pruning | Proposed |
| [0009](0009-git-hook-integration.md) | Git Hook Integration: pre-commit and poly Catalogs | Accepted |
| [0010](0010-distribution-and-naming.md) | Distribution and Naming: Five Channels, One Binary | Accepted |

## Conventions

- Files are named `NNNN-kebab-title.md`, numbered from `0001`, and never renumbered.
- The H1 is `# NNNN — Title` with an em dash. Metadata is a plain bullet list directly under
  it: `Status`, `Date`, then any `Updated:` lines in chronological order.
- Sections are always `## Context`, `## Decision`, `## Consequences`, `## Alternatives
  considered`, in that order. Consequences splits into `Positive:` and `Negative / risks:`
  — naming the costs is the point of the section, so it is never omitted.
- Alternatives are bullets of the form `- **Option:** rejected — reason.` Record the
  alternatives that were genuinely considered, especially the one a reader is most likely to
  reach for.
- Status vocabulary: `Proposed`, `Accepted`, `Superseded (by NNNN)`, `Deprecated`. A decision
  that has not been validated in practice stays `Proposed` and says what would move it —
  see 0008.
- **Amend in place, do not renumber.** When an accepted decision evolves, add an
  `Updated: YYYY-MM-DD (what changed)` line and edit the body. Write a new ADR only when the
  decision is genuinely replaced, and mark the old one `Superseded (by NNNN)`.
- Cross-reference in prose as `ADR 0004` — space, no hyphen. Wrap prose at about 95 columns.
- Keep the index table above in sync when adding or re-statusing an ADR.
