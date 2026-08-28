# 0007 — Reporting: Colored, Deterministic, Machine-Readable

- Status: Accepted
- Date: 2026-08-28

## Context

voom deletes without asking (ADR 0006), so the report is not a convenience — it is the only
record of what happened. It has to serve three audiences at once: a person watching a
`$HOME` sweep scroll past, a person reading `--dry-run` output to decide whether to trust the
tool, and a script or hook consuming the result.

Parallel traversal (ADR 0005) produces results in nondeterministic order, which is useless
for diffing two runs or for reviewing a long list. And because the marker-anchoring rule
(ADR 0002) deliberately *skips* things a naive tool would delete, the skips are as
informative as the removals — a user needs to be able to ask "why didn't you take that?" and
get an answer.

## Decision

**Human output by default, `--format json` for machines.** Both are produced from the same
result set, so they cannot disagree.

Human output:

- **Sorted deterministically by path**, always, regardless of completion order. Two runs over
  an unchanged tree produce byte-identical output, which makes the report diffable.
- One line per artifact: path, ecosystem id, reclaimed size, and outcome. Sizes are
  human-readable (`4.2 GB`) with the raw byte count reserved for JSON.
- A summary footer with totals: artifacts removed, bytes reclaimed, artifacts skipped,
  errors, and elapsed time.
- **Skips are shown with their reason** at `--verbose`: the missing marker (ADR 0002), the
  keep policy that held it (ADR 0004), the exclusion that matched, or the filesystem boundary
  that stopped it. The default view shows only a skip count, so the common case stays short.
- Color via `owo-colors` over `anstream`, which handles Windows consoles and redirection.
  Color is suppressed when stdout is not a TTY, when `NO_COLOR` is set, and when
  `--color=never`; it is forced by `--color=always` or `CLICOLOR_FORCE`. Color is never the
  only carrier of meaning — every state is also distinguished by its text.
- A progress indicator (`indicatif`) during the scan phase, on a TTY only, written to stderr
  so it never contaminates piped stdout.

JSON output is a single object with a stable, versioned shape — a `schema_version`, the
resolved roots and configuration provenance, an array of artifacts (path, ecosystem, bytes,
outcome, and for skips a machine-readable reason code), and the same totals as the footer.
Reason codes are enum-like strings, not prose, so hooks can branch on them.

Exit codes:

| Code | Meaning |
| --- | --- |
| `0` | Ran to completion; nothing failed |
| `1` | Ran, but one or more artifacts could not be removed |
| `2` | Usage or configuration error; nothing was scanned |
| `3` | `--dry-run --exit-code` and artifacts were found (for hooks, ADR 0009) |

Diagnostics go to stderr, results to stdout, so `voom ~ --format json | jq` works without
filtering.

## Consequences

Positive:

- Deterministic ordering makes the output reviewable and diffable, which is what turns
  `--dry-run` from a gesture into an actual audit step.
- Surfacing skip reasons is how the anchoring rule becomes legible instead of feeling like
  the tool randomly missed things — it converts ADR 0002 from an invisible policy into an
  explanation.
- One result set behind two renderers means the JSON can never drift from what the human saw,
  which matters when the human output is the only record of a destructive act.
- Distinct exit codes let a pre-commit hook fail on findings (ADR 0009) without parsing text.

Negative / risks:

- Sorting requires buffering the full result set, so peak memory scales with the number of
  artifacts found. Bounded in practice (thousands, not millions) but it forecloses true
  streaming output.
- A `$HOME` sweep can list thousands of lines. The default hides skip detail to compensate,
  but a `--summary`-only mode will likely be wanted.
- A versioned JSON schema is a compatibility commitment; `schema_version` exists so it can be
  broken deliberately rather than accidentally.
- Progress rendering on stderr can interleave badly with a caller's own stderr logging;
  TTY-only emission limits but does not eliminate this.

## Alternatives considered

- **Streaming results as they complete, unsorted:** rejected — nondeterministic output cannot
  be diffed between runs, and the ordering would vary with thread scheduling, which makes
  reviewing a long list impossible.
- **Only human output, with no machine format:** rejected — the hooks in ADR 0009 and any CI
  usage need a parseable result, and screen-scraping colored text is exactly the fragile
  coupling a JSON mode exists to avoid.
- **`--quiet` as the default for a destructive tool:** rejected — silence about what was
  deleted is unacceptable given ADR 0006's no-prompt design. The report *is* the safety
  surface.
- **A TUI or interactive table:** rejected — ADR 0001 keeps voom non-interactive and pipeable.
- **`colored` instead of `owo-colors` + `anstream`:** rejected — `anstream` handles Windows
  console translation and stream detection properly, and this matches the surrounding
  projects' choice.
