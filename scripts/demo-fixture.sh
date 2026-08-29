#!/usr/bin/env bash
# Build the tree `assets/demo.tape` records against.
#
# Kept beside the tape so the demo is reproducible: the GIF in the README is a real voom run
# over a real tree, and anyone should be able to make the same tree and get the same frames.
# Sizes are padded with random bytes so the reported figures look like a working machine's
# rather than like a fixture's.
set -euo pipefail

root="${1:?usage: demo-fixture.sh <directory>}"
rm -rf "$root"
mkdir -p "$root"
cd "$root"

# Rust, proven by the Cargo.toml beside it.
mkdir -p api/target/debug/deps
printf '[package]\nname = "api"\n' > api/Cargo.toml
dd if=/dev/urandom of=api/target/debug/deps/libapi.rlib bs=1m count=340 2>/dev/null
dd if=/dev/urandom of=api/target/debug/api bs=1m count=78 2>/dev/null

# Node.
mkdir -p web/dist/assets
printf '{"name":"web"}\n' > web/package.json
dd if=/dev/urandom of=web/dist/assets/bundle.js bs=1m count=52 2>/dev/null

# Python.
mkdir -p etl/__pycache__ etl/.ruff_cache
printf '[project]\nname = "etl"\nversion = "1"\n' > etl/pyproject.toml
dd if=/dev/urandom of=etl/__pycache__/main.pyc bs=1k count=820 2>/dev/null
dd if=/dev/urandom of=etl/.ruff_cache/cache bs=1k count=96 2>/dev/null

# The point of the whole tool: a `target/` that no Cargo.toml proves. Somebody's data.
mkdir -p notes/target
printf '# research notes, not build output\n' > notes/target/README.md
dd if=/dev/urandom of=notes/target/dataset.parquet bs=1m count=64 2>/dev/null

# A symlinked artifact: found, reported, refused rather than followed.
mkdir -p elsewhere/real-target linked
printf '[package]\nname = "linked"\n' > linked/Cargo.toml
ln -sfn ../elsewhere/real-target linked/target
