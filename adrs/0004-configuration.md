# 0004 — Configuration: Hierarchical TOML, Include/Exclude, Keep Policies

- Status: Accepted
- Date: 2026-08-28

## Context

The built-in catalog (ADR 0003) encodes what is true of an ecosystem everywhere. It cannot
encode what is true of *this* machine or *this* project: that one repository's `build/` is
hand-written, that a release directory must survive for a week, that a particular scratch
path should be swept even though nothing anchors it, or that artifacts under 10 MB are not
worth the rebuild cost of removing.

Those are per-user and per-tree decisions, they need to be version-controllable when they
belong to a repository and machine-local when they belong to a person, and they need to
compose — a rule set at `$HOME` should still apply inside a project that adds its own.

## Decision

Configuration is **hierarchical TOML**, merged from lowest to highest precedence:

1. Built-in defaults (the catalog, ADR 0003).
2. User config — `${XDG_CONFIG_HOME:-~/.config}/voom/config.toml`.
3. Every `voom.toml` found walking **from the scan root down to each candidate**, nearest
   wins. A `voom.toml` therefore governs its own subtree, which is what makes a
   per-repository policy work in a `$HOME`-wide run.
4. `--config <path>`, which replaces steps 2–3 entirely.
5. Command-line flags.

The schema:

```toml
# Top-level keys must precede every table header: TOML assigns a bare key to the table
# most recently opened, so `exclude` written below `[ecosystems]` becomes
# `ecosystems.exclude` and is rejected by `deny_unknown_fields`.

# Paths never scanned or deleted, relative to this file or absolute. Always wins.
exclude = ["~/work/client-x/**", "**/fixtures/**"]

# Extra paths to sweep that no marker anchors. Explicit user intent (ADR 0002).
include = ["~/scratch/build-junk"]

# Ecosystem participation. Names are catalog ids (ADR 0003).
[ecosystems]
enable  = ["python.venv"]      # turn on a `default_on = false` artifact
disable = ["terraform"]        # turn an ecosystem off entirely

# Keep policies. Default scope; overridable per ecosystem and per path.
[keep]
min_age    = "7d"      # never remove an artifact modified more recently
max_size   = "50GB"    # skip anything larger — likely not what the user meant
min_size   = "1MB"     # not worth the rebuild for anything smaller

[keep.rust]            # per-ecosystem override
min_age = "1d"

[[paths]]              # per-path override, glob-matched, later entries win
match   = "~/work/**"
keep    = { min_age = "30d" }
[[paths]]
match     = "~/experiments/**"
ecosystems = { enable = ["node.build"] }
```

Decisions embedded in that schema:

- **`exclude` is absolute.** It is evaluated before classification, it cannot be overridden
  by a more specific `include`, and an excluded directory is not descended into. This gives
  users one rule they can reason about without understanding precedence.
- **`include` is the only route to unanchored deletion**, and it is exactly the explicit
  user configuration ADR 0002 carves out. Paths listed here bypass marker proof, so the
  report labels them `unanchored (from config)`.
- **Keep policies compose by narrowing, not replacing.** A `[[paths]]` entry overrides only
  the keys it sets; the rest inherit. Policies are evaluated per artifact after
  classification and before deletion.
- **Durations and sizes are human strings** (`7d`, `12h`, `50GB`, `1MiB`), parsed strictly —
  an unparseable value is a hard error at load, never a silently ignored default.
- **Unknown keys are rejected.** `serde(deny_unknown_fields)` throughout, so a typo in a
  safety-relevant key like `exclude` fails loudly instead of quietly disabling the guard the
  user thought they had.
- **`voom config show` prints the fully merged, resolved configuration** with the file each
  value came from, because a hierarchical merge nobody can inspect is a hierarchical merge
  nobody can trust.

## Consequences

Positive:

- A repository can commit a `voom.toml` that protects its own quirks, and it keeps working
  when someone sweeps `$HOME` from above — the case that motivates hierarchy at all.
- `min_age` turns voom into something safe to run on a schedule: today's active build is
  never touched, last month's is.
- Per-path overrides mean the ambiguous `default_on = false` entries from ADR 0003 can be
  enabled narrowly (`~/experiments/**`) rather than globally, which is where they are
  actually safe.
- Strict parsing plus `config show` means safety settings fail visibly, which matters far
  more here than configuration convenience.

Negative / risks:

- Five precedence layers is real complexity, and a user debugging "why did it delete that"
  must reach for `config show`. The alternative — a flat config — cannot express the
  `$HOME`-wide-run-with-per-repo-policy case that motivates the tool.
- Discovering `voom.toml` on the way down costs a `stat` per directory. ADR 0005 folds this
  into the marker lookup the walker already performs.
- `include` is a loaded foot-gun by design. It must be prominent in the README's safety
  section and visibly labelled in reports.
- Human-string durations and sizes invite ambiguity (`1MB` vs `1MiB`); the parser must be
  explicit about both and the docs must show both.

## Alternatives considered

- **Flags only, no configuration file:** rejected — keep policies and per-path rules are far
  too long-lived to retype, and a repository cannot ship its own protections.
- **A single global config file, no hierarchy:** rejected — it cannot express "this one
  repository is different" during a `$HOME`-wide run, which is voom's primary use case.
- **`.voomignore` files in gitignore syntax:** rejected — a second syntax for one narrow
  purpose, and it could only express exclusion, not keep policies or ecosystem selection.
  Exclusion lives in the same TOML as everything else.
- **YAML or JSON:** rejected — TOML matches the surrounding toolchain (`Cargo.toml`,
  `poly.toml`, `pyproject.toml`) and has no significant-whitespace or type-coercion traps.
- **Merging `[[paths]]` by replacement rather than by narrowing:** rejected — replacement
  means a user who sets one key silently loses every default, which for a `keep` policy means
  silently losing a safety guard.

## Amendment — 2026-08-28

`include` has shipped (`src/scan.rs`), and the mechanism is worth recording because it was the
design decision, not just the outcome.

**`include` is matched by the walker, exactly like `exclude`, not injected into the pipeline
afterwards.** A path the walk visits and matches an `include` pattern is turned into a
`Provenance::Included` finding on the spot and never descended into; nothing downstream of the
walk treats an included path any differently from an anchored one. This keeps every deletion
target a path the walk actually produced, so containment (ADR 0006) needs no special case for
unanchored removals — the same "resolves strictly below a scan root" check applies uniformly.
The direct consequence is that **an `include` pattern naming a path outside the scan root is
simply never walked, and therefore never removed.** A user running `voom ~/projects` with
`include = ["~/scratch"]` in scope will not have `~/scratch` swept — the pattern only matches
paths the walker visits under the given roots. This is worth stating plainly because it is the
opposite of what the flat schema above suggests to a first-time reader.

Ordering inside the walker's callback is fixed and matters: **`exclude` is checked first**
(consistent with "exclude is absolute" above), **then `include`, then the dependency-directory
prune.** Checking `include` before the prune is what lets an explicit `include` reach into
`node_modules/` or another dependency directory — the escape hatch ADR 0001 describes for users
who want them gone. `.git` (and every VCS directory) stays unreachable regardless of this
ordering: the deletion guard (ADR 0006) refuses a VCS directory by name whatever produced the
finding, so `include` cannot be used to route around it.

`Finding` carries a `Provenance` enum (`Anchored { artifact, marker_dir }` or
`Included { pattern }`) rather than an `Option<ArtifactId>`, specifically so both reporters must
match on it exhaustively and no output path can be added that silently forgets the label. The
human renderer prints `unanchored` in the column an ecosystem name would otherwise occupy, and
appends `(from config `pattern`)` to the outcome line so the pattern that authorized the removal
is visible next to it. JSON sets `"source": "config-include"` with `ecosystem`, `artifact` and
`marker_dir` all `null`, and adds an `include_pattern` field carrying the matched pattern — a
consumer branches on `source`, not on nullness, because a null `ecosystem` means "nothing proved
this," not "unknown ecosystem."
