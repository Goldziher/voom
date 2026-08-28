---
priority: high
---

# Code style

## Rust

- **Edition 2024.** Line width 120 (`rustfmt.toml`), enforced by poly.
- `cargo clippy --all-targets -- -D warnings` is the bar. Pedantic lints are on; when one is
  genuinely wrong for a case, `#[expect(lint, reason = "…")]` it **at the narrowest scope** —
  never crate-wide. `#[expect]` over `#[allow]` so the exemption fails once it stops being
  needed.
- **No `unwrap()` or `expect()` outside tests.** In a tool that deletes directories, a panic
  mid-run leaves the user with a partial result and no report. Return errors.
- **`thiserror` in the library, `anyhow` at the binary boundary.** Callers of `voom::` get
  typed errors they can match on; `main.rs` adds context and maps to exit codes.
- Prefer `&Path` / `&OsStr` over `String` in anything path-shaped — conversion loses
  information on non-UTF-8 filenames, which exist and which a filesystem tool will meet.
- Errors that describe a filesystem operation always name the path.

## Comments

Comments explain *why*, not *what*. The two places in this codebase that genuinely need a
comment are the walker's disabled-ignore configuration and the anchor logic — both look
wrong to a reader who does not know the reasoning, and both have a comment saying so. Keep
those. Do not narrate ordinary code.

`SAFETY`, `NOTE`, `HACK`, and `WORKAROUND` markers are preserved by the lint configuration;
use them deliberately.

## Dependencies

- Every new dependency needs a reason that survives the question "what would we write
  instead?" Adding one to save twenty lines is usually a bad trade in a tool that must build
  fast and audit cleanly.
- No GPL. `cargo deny` runs in CI and the licence allow-list is explicit.
- Prefer crates already in the workspace's orbit (`clap`, `ignore`, `rayon`, `owo-colors`,
  `anstream`, `serde`) over near-equivalents — consistency across the projects is worth more
  than a marginally nicer API.
- Pin nothing by git. Everything comes from crates.io.

## Documentation

- Public items in the library carry doc comments. The catalog entry type and the anchor enum
  especially — they are the API a contributor reads before adding an ecosystem.
- Documentation that makes a claim about behaviour (an ecosystem table, a flag's default)
  must match the code. `voom catalog` prints `CATALOG` directly and cannot drift.
  `tests/docs.rs` checks part of the README against it — the ecosystem list, the markers the
  table advertises, and every `voom.toml` example, which it loads through the real config
  pipeline. Everything else in the README is prose nothing verifies, so a claim you add there
  is a claim you are maintaining by hand.

## Commits

Conventional commits, signed. See [[tdd-and-hooks]] for the pre-commit gate.

Related: [[architecture]], [[perf-discipline]].
