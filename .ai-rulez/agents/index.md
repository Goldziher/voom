# Concepts

* [Catalog Author](catalog-author.md) - Adds a new language ecosystem to voom's build-artifact catalog end to end — researches the ecosystem's real artifact layout and marker files, writes the catalog entry with a correct anchor, builds both the positive and the negative fixture, and updates the README table and ADR 0003.
* [Rust Perf Engineer](rust-perf-engineer.md) - Reviews diffs touching voom's traversal, classification, sizing, or deletion paths for hot-path regressions — re-walks and redundant stats, allocations in the classify loop, lost subtree pruning, glob recompilation, and any change that weakens the walker's disabled-ignore configuration.
