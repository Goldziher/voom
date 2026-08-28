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

pub mod caches;
pub mod catalog;
pub mod classify;
pub mod error;
pub mod names;
pub mod scan;

pub mod cli;
pub mod config;
pub mod delete;
pub mod policy;
pub mod report;
pub mod run;
pub mod size;
// The integration suites cannot see a `#[cfg(test)]` module of the library they link
// against, and two copies of a fixture helper drift — a snapshot that handled symlinks or
// separators differently in one suite would have it trusting output the other could not
// produce. The `test-support` feature exposes the one copy; `cargo test` turns it on through
// the self dev-dependency.
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod watch;
