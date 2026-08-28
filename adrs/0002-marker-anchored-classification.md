# 0002 — Marker-Anchored Classification: Never Delete on Directory Name Alone

- Status: Accepted
- Date: 2026-08-28

## Context

Build-artifact directory names are not unique to their ecosystem, and several of the most
common ones are also ordinary source directory names. A survey of one workspace tree
(`~/workspace`, depth 4, excluding `node_modules`) found:

| Directory name | Occurrences | Artifact in… | Source in… |
| --- | --- | --- | --- |
| `bin` | 41 | .NET, Crystal, Go install targets | almost every project's hand-written scripts |
| `vendor` | 20 | — | Go vendoring, composer dependencies |
| `obj` | 12 | .NET | occasionally hand-managed C object dirs |
| `_build` | 12 | Elixir, OCaml/dune | — |
| `target` | 3 | Rust, Maven | — |
| `build` | 1 | CMake, Gradle, Python, Dart | many projects' build *scripts* directory |

A tool that matches on name alone deletes hand-written `bin/` and `vendor/` directories the
first time it is pointed at a mixed tree. Since voom's stated goal (ADR 0001) is to be
runnable against `$HOME`, and since it deletes by default (ADR 0006), a single such false
positive is unrecoverable data loss.

The distinguishing signal is always nearby: a `target/` that belongs to Rust has a
`Cargo.toml` next to it; an `obj/` that belongs to .NET has a `*.csproj` beside or above it;
a `.venv/` that belongs to Python sits next to a `pyproject.toml` or `setup.py`. The
ecosystem proves itself with a **marker file**.

## Decision

**A directory is an artifact only when a marker file proves its ecosystem at a declared
relative position.** The name alone is never sufficient.

Every entry in the built-in catalog (ADR 0003) declares three things:

1. **`artifacts`** — the paths to remove, relative to the anchor.
2. **`markers`** — the file globs that prove the ecosystem.
3. **`anchor`** — where the marker must be found relative to the candidate:
   - `sibling` — the marker sits in the same directory as the artifact. This is the default
     and covers most ecosystems (`target/` ← `Cargo.toml`).
   - `ancestor(n)` — the marker may sit up to `n` levels above, for ecosystems that nest
     artifacts inside a project (`obj/` ← a `*.csproj` anywhere in the project root chain).

Classification of a candidate path therefore requires a positive marker match. No match, no
deletion — the candidate is reported as skipped, with the reason, when `--verbose` is set.

Three rules follow from this and are binding:

- **The built-in catalog may never contain a name-only entry.** An entry without at least one
  marker glob is a bug, and is rejected at construction time by a unit test over the whole
  catalog.
- **Name-only matching is reachable only through explicit user configuration** — a user who
  writes `paths = ["~/scratch/junk"]` or an unanchored custom rule in `voom.toml` has said
  what they mean and owns the result. voom never infers it.
- **Every catalog entry ships a false-positive test**: a fixture tree containing the artifact
  directory *without* its marker, asserting that voom leaves it alone.

Ambiguous names resolve by ecosystem, not by precedence — `target/` next to a `Cargo.toml` is
Rust's, `target/` next to a `pom.xml` is Maven's, and `target/` next to neither is nobody's
and survives.

## Consequences

Positive:

- The 41 `bin/`, 20 `vendor/`, and 12 `obj/` directories in the survey above are all correctly
  left alone, because the overwhelming majority have no proving marker.
- The rule is simple enough to state in the README in two sentences, which is what makes the
  "safe against `$HOME`" claim credible to a reader who has been burned before.
- Anchoring gives a natural, honest `--verbose` explanation for every decision: the marker
  that proved it, or the marker that was missing.
- It makes catalog entries reviewable. A pull request adding an ecosystem is judged on
  whether its marker genuinely proves the ecosystem, which is a concrete question.

Negative / risks:

- **Recall drops.** A `target/` left behind by a since-deleted Rust project has no `Cargo.toml`
  anymore, so voom will not reclaim it. This is the correct trade — an orphaned artifact
  costs disk, a deleted source directory costs work — but users will notice and should be
  told why.
- Marker lookup costs at least one extra `stat` (often a small `read_dir`) per candidate
  directory. ADR 0005 addresses this by resolving markers from the directory listing the
  walker already produced, rather than re-walking.
- `ancestor(n)` needs a bound and a cache, or a deep tree re-checks the same ancestors
  repeatedly. The classifier memoizes marker resolution per directory.
- Ecosystems whose marker is itself generated (some CMake layouts) fit poorly and may need
  per-entry special-casing.

## Alternatives considered

- **Name-only matching with a curated denylist:** rejected — the denylist can never be
  complete, and its failure mode is silent data loss rather than a missed cleanup. This is
  precisely how the naive `find -name target -exec rm -rf` loses work.
- **Heuristic content inspection** (treat a directory as an artifact if it contains only
  binaries, or if nothing in it is tracked by git): rejected — expensive (requires reading
  into every candidate), unreliable across ecosystems, and impossible to explain to a user in
  a one-line report.
- **Deferring to `.gitignore`** — treat anything git ignores as disposable: rejected — far
  too broad. `.gitignore` routinely lists `.env`, local configuration, editor state, and
  scratch data that is not regenerable. See also ADR 0005, which rejects `.gitignore` for the
  opposite reason on the traversal side.
- **Requiring the user to confirm each ambiguous directory:** rejected — an interactive prompt
  cannot run from a hook or CI (ADR 0001), and prompt fatigue converts into reflexive
  approval, which is worse than not asking.
