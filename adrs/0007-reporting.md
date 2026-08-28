# 0007 — Reporting: Colored, Deterministic, Machine-Readable

- Status: Accepted
- Date: 2026-08-28
- Updated: 2026-08-28 — `--summary` shipped; noted the two labels other ADRs added; the
  human layout was rebuilt around blocks and root-relative paths.

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
  human-readable (`4.2 GB`) with the raw byte count reserved for JSON. The layout this became
  is described in the amendment below.
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
  and `--summary` prints the footer alone.
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

## Amendment — 2026-08-28

`--summary` has shipped, closing the "will likely be wanted" risk above. It prints the totals
footer and nothing else, and it also suppresses the scan progress indicator — a spinner is only
useful to someone watching a list scroll past, and there is no list.

Two report states were specified by other ADRs after this one was accepted, and are recorded
there rather than duplicated here:

- **`unanchored`**, for a removal authorized by a config `include` rather than by a marker.
  ADR 0004's amendment defines the `Provenance` enum behind it and both renderings.
- **`interrupted`**, for a walk failure the walker judged transient rather than a real
  inability to read a directory. ADR 0005's amendment explains the distinction and why the
  transient case is hidden unless `--verbose` asks for it.

Both exist for the same reason the skip reasons do: a destructive tool's report is its safety
surface, so a state the reader would interpret differently gets its own label rather than being
folded into a neighbouring one.

## Amendment — 2026-08-28 (layout)

The one-line-per-artifact shape above survives; how it is laid out does not.

A `$HOME` sweep prints on the order of two hundred absolute paths, and absolute paths wrap. Two
hundred findings became roughly four hundred terminal rows, which is not a report anyone reads.
The renderer now:

- names the scan root once in a header and prints every path **relative to it** — or to the
  deepest common ancestor when several roots were given. A path outside every root, which a
  configured `include` can produce, still prints in full;
- emits three blank-line-separated **blocks** — artifacts, skips, walk failures — each with
  column widths measured over its own rows. A skip has no size and no ecosystem, so in a shared
  table it rendered as a row with two holes in it;
- introduces a reason with an aligned em dash rather than parentheses, because the reasons
  themselves contain parentheses (`no rust marker (Cargo.toml) proves it`);
- splits the footer into a headline, a per-ecosystem breakdown ordered by bytes, and separate
  lines for skipped, refused and failed.

**The artifact list is deliberately not truncated.** A `+N more` line is the obvious answer to
volume and it is the wrong one here: this report is the only record of a destructive act, and
the reader most likely to be scrolling it is the one checking what voom just deleted. Width was
the actual problem, and width is what was fixed. Should a cap ever be wanted it belongs behind
an explicit flag, capping only removals and never a refusal or a failure.

**Colour.** Refused moved from blue to cyan: plain ANSI blue is the one standard colour that is
unreadable against dark backgrounds in common themes, and it was carrying the state a reader
most needs to notice. Ecosystem moved to dim, which also lets the magenta `unanchored` marker
carry further. Every state is still named in words, so colour remains a second channel and
never the only one.

One consequence worth recording: `Outcome::Failed` no longer embeds the path in its message,
because every renderer already names it on the same line. That changes the JSON `detail` field,
which now holds the underlying error alone — `path` is in the same object.
