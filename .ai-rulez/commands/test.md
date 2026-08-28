---
priority: high
usage: "/test [filter]"
description: "Run the test suite and report failures, with attention to the safety fixtures"
---

# Test

Run the suite, optionally filtered by `${1:-}`.

1. `cargo test ${1:-}` — unit, integration, and fixture-tree tests.
2. If snapshots differ, show the diff. Do **not** run `cargo insta accept` without being
   asked — a changed report snapshot is a behaviour change that wants review.

Report:

- Pass/fail counts and the name of every failing test.
- For each failure: the assertion, the expected and actual, and which stage of the pipeline it
  covers (scan, classify, policy, delete, report).

## Call out safety failures explicitly

These tests exist because their failure mode is data loss or silent uselessness. If one of
them fails, say so at the top of the report rather than in a list:

- A **negative catalog fixture** (artifact present, marker absent, must survive) — a failure
  here means voom would delete a directory nobody proved was build output.
- The **gitignored `target/` discovery test** — a failure means the walker started respecting
  `.gitignore` and voom now finds nothing.
- **Symlink refusal**, **containment**, or **protected-path denylist** tests — a failure means
  a safety rail from ADR 0006 is gone.

Never "fix" one of these by changing the test to match the new behaviour. The test is the
specification.
