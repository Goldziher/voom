---
priority: medium
usage: "/build [args]"
description: "Build voom and report warnings — debug by default, release with --release"
---

# Build

Build the crate over `${1:-}`.

1. `cargo build ${1:-}` — debug build unless the caller passed `--release`.
2. If it fails, report the first error with its file and line, and stop. Do not attempt a fix
   unless asked.

Report:

- Pass or fail, and the build profile used.
- Every warning, grouped by crate and kind. Warnings are not acceptable noise here —
  `cargo clippy --all-targets -- -D warnings` is the CI bar, so a warning now is a failure
  later.
- Wall-clock time for a release build, which is worth watching: voom ships five platform
  binaries per release.

For a full check before committing, use `/poly-check` and run `cargo test` — this command
only compiles.
