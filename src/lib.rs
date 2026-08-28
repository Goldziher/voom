//! voom — fast, safe, parallel build-artifact pruning.
//!
//! The library half of the `voom` binary. Everything testable lives here; `main.rs` is a
//! thin shell that parses arguments, dispatches, and maps errors to exit codes.
//!
//! The pipeline is `scan -> classify -> policy -> delete -> report`; see
//! `adrs/0001-mission-and-scope.md` for scope and the ADRs it references for each stage.
//!
//! Two invariants carry the whole design and are covered by regression tests:
//!
//! - The directory walker runs with **all** ignore handling disabled — build artifacts are
//!   precisely what `.gitignore` lists (`adrs/0005-traversal-and-parallelism.md`).
//! - Nothing is deleted without a marker file proving its ecosystem
//!   (`adrs/0002-marker-anchored-classification.md`).

/// The version of the `voom` crate, as published.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
