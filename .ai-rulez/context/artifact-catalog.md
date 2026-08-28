---
priority: high
---

# The artifact catalog

The catalog is the table that says which directories are build output for which ecosystem.
It is a `const` array compiled into the binary (`src/catalog/`), not a file loaded at
runtime. See `adrs/0003-builtin-catalog.md`.

## The shape of an entry

```rust
pub(super) const RUST: Ecosystem = Ecosystem {
    id: "rust",                       // stable slug used in config and reports
    name: "Rust",                     // human label
    markers: &["Cargo.toml"],         // proves the ecosystem — at least one, always
    anchor: Anchor::Sibling,          // or Anchor::Ancestor(3)
    artifacts: &[Artifact::on("target/")],
};
```

`Artifact` is built through const constructors, never by naming its fields:

| Constructor | Meaning |
| --- | --- |
| `Artifact::on(path)` | participates by default |
| `Artifact::off(path, note)` | needs an explicit opt-in; the note says why |
| `.at(Anchor::Ancestor(1))` | overrides the ecosystem's anchor for this artifact alone |
| `.named("zig-cache-legacy")` | overrides the slug, for the rare pair that derives the same one |

- **`markers`** is what makes deletion legitimate. An entry with an empty `markers` list is
  rejected by a construction-time test — see [[safety-first]].
- **`anchor`** says where the marker may live relative to the candidate. `Sibling` covers
  most ecosystems (`target/` ← `Cargo.toml`). `Ancestor(n)` is for artifacts nested inside a
  project, like .NET's `obj/` under a `*.csproj` that may be several levels up.
- **`default_on: false`**, written as `Artifact::off`, is for artifacts whose name is also a
  plausible source directory within that same ecosystem (`build/` under a `package.json`,
  `bin/` under a `go.mod`) or whose removal is expensive rather than cheap (`.venv/`). The
  note is mandatory — a test asserts it — and `voom catalog` prints it.
- **The slug** an artifact answers to in configuration (`python.venv`, `node.build`) is
  *derived* from `path`, not declared, so it cannot drift from it. `.named()` exists only for
  the collision case.

## Adding an ecosystem

1. **Confirm the marker actually proves the ecosystem.** This is the whole review question.
   `build.zig` proves Zig. `Makefile` proves nothing — half the world has one.
2. Add the entry to the right module under `src/catalog/`, and to `CATALOG` in
   `src/catalog/mod.rs` — an entry that is not in the array is not in the tool.
3. Run `cargo test`. Both fixtures — marker present → artifact removed, marker absent →
   artifact untouched — are generated from `CATALOG`, so they appear on their own
   ([[tdd-and-hooks]]). Add a hand-written test only if the anchor climbs.
4. If the artifact name is ambiguous, use `Artifact::off` with a `note` rather than enabling
   it and hoping.
5. Update the README's ecosystem table and `adrs/0003-builtin-catalog.md`. `tests/docs.rs`
   fails if the README's first column and the catalog disagree, and if the README advertises a
   marker the catalog does not have — but it will not notice a marker or artifact the README
   *omits*, so check the other direction yourself.

## What goes in only as an opt-in

Dependency directories: `node_modules/`, composer `vendor/`, Go `vendor/`, mix `deps/`.
These are network-fetched caches, not build output — removing them costs the user a
re-download and can break offline work. ADR 0001's amendment lets them into the table, but
**only ever as `Artifact::off` with a note**. `--clean-dependencies` enables the group
(`catalog::DEPENDENCY_ARTIFACTS`) and is pure sugar over `--enable`; a default run sweeps none
of them, and the walker still never descends into one.

`bower_components/` is the one that stays out entirely: it is pruned like the others, but no
marker distinguishes a Bower install from any other directory of that name, so ADR 0002 gives
us nothing to write.

Machine-global tool caches and installed toolchains (`~/.cargo/registry`, `~/.npm`, `~/.pyenv`,
`~/google-cloud-sdk`) are likewise not catalog entries. They are excluded by *location* instead,
in `CACHE_DIRS` (`src/caches.rs`), because marker anchoring cannot tell an installed program
from a project that was built in place — an installed pnpm really does sit beside a real
`package.json`. `--caches` opts back in. That list is append-only for the same reason the
protected-path denylist is; see the ADR 0001 amendment.

## Invariants the tests enforce

In `src/catalog/mod.rs`, over the whole table:

- Every entry has at least one marker, and none of them is empty.
- Every entry declares at least one artifact, and every `id` is unique.
- No artifact path is absolute, home-relative, or contains a `..` or `.` segment — it must
  stay under its anchor.
- No artifact matching a known dependency directory name is on by default, and each carries a
  note (`vendor/bundle/` is unaffected: it names Ruby's build output *inside* `vendor/`, not
  `vendor/` itself).
- Every off-by-default artifact has a `note`.
- The catalog fits the classifier's `u32` marker bitmask (`MAX_ECOSYSTEMS` = 32 entries).
- No ancestor anchor climbs more than 8 levels.
- Every leading segment of a multi-segment artifact path is a literal — the classifier keys on
  the last segment and byte-compares the ones above it, which is what keeps the hot path
  allocation-free.
- Artifact slugs are unique within an ecosystem and never empty.

Related: [[architecture]], [[safety-first]].
