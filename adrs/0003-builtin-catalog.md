# 0003 — Built-in Catalog: Data-Driven Ecosystem Definitions

- Status: Accepted
- Date: 2026-08-28

## Context

ADR 0002 requires every artifact to be proven by a marker file at a declared anchor position.
That turns "which directories are artifacts" into a data question: a table of
`(ecosystem, markers, artifacts, anchor)` rows. The table needs to cover the ecosystems a
working developer actually has on disk — the survey in ADR 0001 found 16 in a single
workspace tree — and it needs to grow without touching the traversal or deletion code.

Two properties are in tension. The catalog should be **exhaustive**, because an ecosystem
voom does not know is disk it cannot reclaim. It should also be **conservative**, because
each entry is a licence to delete and ADR 0002's whole point is that a wrong entry is a
data-loss bug.

## Decision

The catalog is a **static, data-driven table compiled into the binary** — a `const` array of
entries in `src/catalog/`, not a file loaded at runtime. Entries are pure data; the
classifier is the only code that reads them.

Each entry declares:

| Field | Meaning |
| --- | --- |
| `id` | Stable slug used in config and reports (`rust`, `dotnet`, `gradle`) |
| `name` | Human label for reporting |
| `markers` | Globs that prove the ecosystem — **at least one is mandatory** |
| `anchor` | `Sibling`, or `Ancestor(n)` for artifacts nested inside a project |
| `artifacts` | Paths to remove, relative to the anchor directory |
| `default_on` | Whether the entry participates without explicit opt-in |
| `note` | Why an entry is off by default, surfaced in `voom catalog` |

The initial catalog covers **24 ecosystems**:

| id | Markers | Artifacts | Anchor |
| --- | --- | --- | --- |
| `rust` | `Cargo.toml` | `target/` | sibling |
| `node` | `package.json` | `dist/`, `.next/`, `.nuxt/`, `.svelte-kit/`, `.astro/`, `.turbo/`, `.parcel-cache/`, `.vite/`, `.nyc_output/`, `*.tsbuildinfo`, `build/`† | sibling |
| `python` | `pyproject.toml`, `setup.py`, `setup.cfg` | `__pycache__/`, `.pytest_cache/`, `.mypy_cache/`, `.ruff_cache/`, `.tox/`, `*.egg-info/`, `htmlcov/`, `.coverage`, `build/`, `dist/`, `.venv/`† | sibling / ancestor(1) for `__pycache__` |
| `go` | `go.mod` | `bin/`† | sibling |
| `zig` | `build.zig` | `.zig-cache/`, `zig-cache/`, `zig-out/` | sibling |
| `swift` | `Package.swift` | `.build/` | sibling |
| `xcode` | `*.xcodeproj`, `*.xcworkspace` | `DerivedData/` | sibling |
| `maven` | `pom.xml` | `target/` | sibling |
| `gradle` | `build.gradle`, `build.gradle.kts`, `settings.gradle*` | `build/`, `.gradle/` | sibling |
| `dotnet` | `*.csproj`, `*.fsproj`, `*.vbproj`, `*.sln` | `bin/`, `obj/` | ancestor(3) |
| `cmake` | `CMakeLists.txt` | `build/`, `cmake-build-*/`, `CMakeFiles/` | sibling |
| `dart` | `pubspec.yaml` | `.dart_tool/`, `build/` | sibling |
| `elixir` | `mix.exs` | `_build/`, `.elixir_ls/` | sibling |
| `ruby` | `Gemfile`, `*.gemspec` | `.bundle/`, `vendor/bundle/`, `coverage/`, `pkg/`†, `tmp/`† | sibling |
| `php` | `composer.json` | `.phpunit.cache/`, `.phpunit.result.cache` | sibling |
| `scala` | `build.sbt` | `target/`, `project/target/`, `.bloop/`, `.metals/` | sibling |
| `haskell` | `*.cabal`, `stack.yaml` | `dist-newstyle/`, `.stack-work/` | sibling |
| `ocaml` | `dune-project` | `_build/` | sibling |
| `julia` | `Project.toml` | `deps/build/`† | sibling |
| `r` | `DESCRIPTION` | `*.Rcheck/`, `src/*.o`, `src/*.so` | sibling |
| `nim` | `*.nimble` | `nimcache/` | sibling |
| `elm` | `elm.json` | `elm-stuff/` | sibling |
| `terraform` | `*.tf` | `.terraform/` | sibling |
| `bazel` | `WORKSPACE`, `WORKSPACE.bazel`, `MODULE.bazel` | `bazel-*` | sibling |

† = `default_on = false`. An artifact is off by default when the name is also a common
source directory even inside that ecosystem (`build/` under `package.json`, `bin/` under
`go.mod`, `pkg/` and `tmp/` under a `Gemfile`) or when removal is expensive rather than
cheap (`.venv/`, which costs a reinstall). Users enable them per-ecosystem or per-path in
`voom.toml` (ADR 0004).

**Dependency directories are absent by construction**, per ADR 0001: `node_modules/`,
composer's `vendor/`, mix's `deps/`, and Go's `vendor/` are not in the table at all and no
flag turns them on. Reaching them requires the user to name a path explicitly.

The catalog is guarded by construction-time tests that assert: every entry has ≥1 marker
(enforcing ADR 0002), every `id` is unique, no artifact path escapes its anchor via `..` or
an absolute component, and no artifact matches a dependency directory name.

## Consequences

Positive:

- Adding an ecosystem is a data change plus fixtures — reviewable on the one question that
  matters ("does this marker actually prove this ecosystem?") and impossible to get subtly
  wrong in the traversal code.
- Compiling the table in keeps startup free of I/O and makes the catalog part of the tested,
  versioned artifact rather than machine state that can drift.
- `default_on = false` gives a principled home for the genuinely ambiguous entries instead of
  forcing a binary include/exclude decision that would either lose recall or risk source.
- `voom catalog` can print the whole table, so the tool documents its own behaviour and the
  README table cannot drift from the code.

Negative / risks:

- A compiled-in catalog means a new ecosystem requires a release. Acceptable: ADR 0004's
  configuration lets a user define a custom rule locally in the meantime.
- Twenty-four entries is already more than one person can hold in their head; the invariant
  tests, not review attention, have to be what keeps it correct.
- The `†` defaults will read as under-delivery to users who expect `build/` swept everywhere.
  The `--verbose` skip reason and `voom catalog` must make the opt-in obvious.
- `ancestor(3)` for .NET is a guess at real project depth; too small misses artifacts, too
  large risks matching across unrelated projects. It needs fixtures at each depth.

## Alternatives considered

- **A runtime-loaded catalog file** (TOML shipped alongside the binary, or fetched):
  rejected — introduces a file that can go missing or drift from the binary, costs startup
  I/O, and turns a data-integrity invariant into a runtime error. User configuration
  (ADR 0004) already provides the extensibility this would buy.
- **Deriving the catalog from each ecosystem's own tooling** (asking `cargo metadata` for
  `target-dir`, etc.): rejected for the same reason ADR 0001 rejected shelling out — it
  requires every toolchain installed and destroys the performance goal.
- **One flat list of artifact names with no ecosystem grouping:** rejected — grouping is what
  makes per-ecosystem configuration, per-ecosystem reporting, and the ADR 0002 anchor
  declaration possible.
- **Including `node_modules/` behind a flag:** rejected — a flag that deletes tens of
  gigabytes of network-fetched dependencies belongs to a different tool with a different
  safety story. Naming the path explicitly remains possible for users who want it.
- **Treating every `†` entry as on by default and relying on the report to catch mistakes:**
  rejected — deletion is immediate (ADR 0006), so a report that arrives after the fact is not
  a control.
