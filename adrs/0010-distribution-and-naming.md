# 0010 — Distribution and Naming: Five Channels, One Binary

- Status: Accepted
- Date: 2026-08-28

## Context

voom's audience is defined by ecosystem breadth (ADR 0003), not by language: the person with
a Gradle project and a Python project and a Zig project on one disk is exactly the target
user, and they should not need a Rust toolchain to install a cleanup tool. Reaching them
means meeting them in whichever package manager they already have.

The working name for this project was `pruner`, and it turned out to be unavailable
everywhere that matters. `pruner` on crates.io is an actively maintained backup-pruning tool
(32k downloads, last published 2026-06-12) — a real project, not a squat. `pruner` on npm and
`pruner` on PyPI are both abandoned 2022 packages, and `pruner-cli` on npm is likewise taken.
The name had to change before anything could ship.

## Decision

voom is distributed through **five channels from one statically-linked binary**, following
the pipeline uncomment already proves out. There is no cargo-dist, no goreleaser, and no
release-plz: a single hand-written `publish.yaml` plus one bash script for the Homebrew
formula.

| Channel | Package name | Install |
| --- | --- | --- |
| Homebrew | `Goldziher/homebrew-tap` → `Formula/voom.rb` | `brew install goldziher/tap/voom` |
| crates.io | `voom` | `cargo install voom` |
| npm | `@goldziher/voom` | `npm install -g @goldziher/voom` |
| PyPI | `voom-cli` | `pip install voom-cli` |
| GitHub releases | `voom-<triple>.tar.gz` / `.zip` | `cargo binstall voom` |

**The package names are deliberately asymmetric, and this is a scar, not a design.** The
binary on `$PATH` is `voom` in every case; only the package identifiers differ:

- crates.io took `voom` cleanly.
- PyPI's `voom` was taken by an unrelated 2022 project, so the distribution is `voom-cli`
  while the import package and console script remain `voom`.
- npm's `voom` was taken, and `voom-cli` was **rejected by npm's similarity check** as too
  close to the abandoned `voomcli`. A scoped name bypasses that check entirely, so npm gets
  `@goldziher/voom`.

The name itself comes from *The Cat in the Hat Comes Back*: Voom is what Little Cat Z pulls
out of his hat to clear every last speck of pink snow at once.

Release mechanics:

- Five targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-gnu`
  (mingw cross-compiled on Linux), and `x86_64-unknown-linux-gnu` /
  `aarch64-unknown-linux-gnu` (built inside `manylinux_2_28` for glibc compatibility).
- The GitHub release is created as a **draft** and only published once all five archives and
  the checksums file verify, so a partial release is never visible.
- Authentication is OIDC trusted publishing for crates.io, npm, and PyPI. Each is bound to
  the **exact workflow filename**, which is why the workflow is `publish.yaml` and must never
  be renamed. The only long-lived secret is `HOMEBREW_TOKEN`.
- The workflow is **re-runnable**: it probes each registry for the version before publishing
  and skips what already exists, so a failure in one channel does not block the others on
  retry.
- npm and PyPI packages are thin wrappers that download the release binary on **first run**,
  so a single Rust build serves all five channels. See the amendment below for why npm does it
  this way rather than from a `postinstall` script.

**Version is a single value across five surfaces**: `Cargo.toml`, `npm-package/package.json`,
`pip-package/pyproject.toml`, `pip-package/voom/__init__.py`, and `poly-hooks.toml`
(ADR 0009, which pins the version in every channel string). A `scripts/bump-version.sh`
rewrites all five atomically, and CI independently gates the release on their agreement.

## Consequences

Positive:

- A user reaches voom through the package manager they already have, which for a
  cross-ecosystem tool is the difference between being installable and being installed.
- One Rust build feeding all five channels means no per-channel reimplementation and no
  behavioural drift between them.
- OIDC trusted publishing removes three long-lived registry tokens from the repository's
  secret surface.
- Draft-until-verified means users never see a half-uploaded release.

Negative / risks:

- **Three different package names is a documentation burden and a discoverability tax.**
  Someone who reads about `voom` and types `pip install voom` gets an unrelated 2022 package.
  The README install table has to be unambiguous, and it is the only mitigation available.
- The npm and PyPI wrappers fetch a binary at install or first run, so both fail in
  network-isolated environments in a way `cargo install` does not.
- Five version surfaces is a real footgun; the bump script and the CI gate exist precisely
  because uncomment shipped without the former and relies on the latter to catch drift.
- Binding trusted publishing to a filename means the workflow file is effectively immutable
  in name — a rename silently breaks publishing on all three registries at once.
- Windows is cross-compiled with mingw and never built on a Windows runner, so
  Windows-specific regressions are therefore caught by CI's `windows-latest` test leg rather
  than by anything in the release itself; the release's `smoke_test` job unpacks and runs the
  published macOS and Linux archives, which is where cross-compilation is not in play.

## Alternatives considered

- **Keep the name `pruner` and decorate per registry** (`pruner-rs` everywhere): rejected —
  the crates.io `pruner` is an active project, so the bare name would never be available, and
  a permanently decorated name is worse than a clean one.
- **`prunekit` / `buildprune` / `reclaimr`** — all verified free on every registry: rejected
  in favour of `voom`, which is shorter, more memorable, and carries a cleanup metaphor that
  explains itself once.
- **Unscoped npm names** (`voom-prune`, `voom-sweep`): rejected — npm's similarity matcher is
  fuzzy and had already rejected one candidate, so each attempt costs a failed publish. A
  scoped name is checked only within the scope and is therefore deterministic.
- **cargo-dist or goreleaser** to generate the release pipeline: rejected — uncomment's
  hand-written workflow already encodes hard-won fixes (manylinux `CARGO_HOME` paths, the
  npm 11 OIDC version trap, `safe.directory` in containers) that a generator would discard,
  and the pipeline is not changing often enough to justify the abstraction.
- **crates.io only, letting users build from source:** rejected — it restricts a
  cross-ecosystem tool to Rust developers, which is a small subset of the people whose disks
  it would help.
- **Per-platform npm packages via `optionalDependencies`** instead of a downloader: rejected
  for v1 — it is the better mechanism, but it multiplies the publishing matrix by five and
  uncomment's single-package approach is proven. Worth revisiting; the amendment below narrows
  the gap by removing the reason it was most likely to be forced.

## Amendment — 2026-08-28 (npm fetches on first run, not on install)

The npm wrapper downloaded the binary from a `postinstall` script. That does not work on
current npm and would have shipped a channel that simply does not install.

npm 11.19 gates install scripts behind `allow-scripts`, off by default. A blocked `postinstall`
does not merely skip the download: npm declines to create the `bin` link at all, because the
target the manifest names does not exist. Reproduced against a clean prefix with a
shape-identical package — `npm i -g` printed a warning, created no `bin/` entry, and left
`voom: command not found`; `npx -y` exited 0 having done nothing. The setting only gets
stricter from here, and asking every user to pass `--allow-scripts` is not a distribution
channel.

`bin/voom` is now a committed Node launcher that resolves the binary — fetching it on first use
— and hands over to it. `download.js` holds the fetch. Consequences:

- The package works with install scripts disabled, which is the default.
- It fixes a second bug that the first one was hiding: `install.js` wrote `bin/voom.exe` on
  Windows while the manifest linked `bin/voom`, so the shim pointed at a file that would never
  exist. The launcher is a Node script under one name on every platform.
- The binary caches per user (`~/.cache/voom/<version>`, `%LOCALAPPDATA%` on Windows) rather
  than inside the package directory, which a global install often puts somewhere the running
  user cannot write. `VOOM_BINARY` overrides it. This is what the PyPI wrapper already did, so
  the two channels now behave the same way.
- First run pays the download; later runs do not. Unpacking stages into a temporary directory
  and renames, so two concurrent invocations cannot corrupt each other.
