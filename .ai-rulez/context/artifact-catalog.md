---
priority: high
---

# The artifact catalog

The catalog is the table that says which directories are build output for which ecosystem.
It is a `const` array compiled into the binary (`src/catalog/`), not a file loaded at
runtime. See `adrs/0003-builtin-catalog.md`.

## The shape of an entry

```rust
Ecosystem {
    id: "rust",                       // stable slug used in config and reports
    name: "Rust",                     // human label
    markers: &["Cargo.toml"],         // proves the ecosystem — at least one, always
    anchor: Anchor::Sibling,          // or Anchor::Ancestor(3)
    artifacts: &[
        Artifact { path: "target/", default_on: true, note: None },
    ],
}
```

- **`markers`** is what makes deletion legitimate. An entry with an empty `markers` list is
  rejected by a construction-time test — see [[safety-first]].
- **`anchor`** says where the marker may live relative to the candidate. `Sibling` covers
  most ecosystems (`target/` ← `Cargo.toml`). `Ancestor(n)` is for artifacts nested inside a
  project, like .NET's `obj/` under a `*.csproj` that may be several levels up.
- **`default_on: false`** is for artifacts whose name is also a plausible source directory
  within that same ecosystem (`build/` under a `package.json`, `bin/` under a `go.mod`) or
  whose removal is expensive rather than cheap (`.venv/`). Always give these a `note` — it is
  printed by `voom catalog` and explains the opt-in.

## Adding an ecosystem

1. **Confirm the marker actually proves the ecosystem.** This is the whole review question.
   `build.zig` proves Zig. `Makefile` proves nothing — half the world has one.
2. Add the entry to the right module under `src/catalog/`.
3. Write **both** fixtures ([[tdd-and-hooks]]): marker present → artifact removed; marker
   absent → artifact untouched. The second one is the point.
4. If the artifact name is ambiguous, set `default_on: false` with a `note` rather than
   enabling it and hoping.
5. Update the README's ecosystem table and `adrs/0003-builtin-catalog.md`.

## What never goes in

Dependency directories: `node_modules/`, composer `vendor/`, Go `vendor/`, mix `deps/`.
These are network-fetched caches, not build output — removing them costs the user a
re-download and can break offline work. ADR 0001 puts them out of scope and no flag turns
them on. Users who want them gone name the path explicitly in their own config.

Global tool caches (`~/.cargo/registry`, `~/.npm/_cacache`, `~/.gradle/caches`) are likewise
out of scope for v1 — they are machine-global, shared across projects, and have their own
eviction tooling.

## Invariants the tests enforce

- Every entry has at least one marker.
- Every `id` is unique.
- No artifact path contains `..` or an absolute component — it must stay under its anchor.
- No artifact matches a known dependency directory name.

Related: [[architecture]], [[safety-first]].
