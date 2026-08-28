# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Project scaffolding: architecture decision records, contributor governance, `poly`
  configuration, packaging for five distribution channels, and CI.
- `0.0.0` name placeholders published to crates.io (`voom`) and PyPI (`voom-cli`) to reserve
  the names ahead of the first functional release.

### Notes

- The project was renamed from its working title `pruner`, which was unavailable on
  crates.io, npm, and PyPI. See
  [ADR 0010](adrs/0010-distribution-and-naming.md) for the full record, including why the
  package names differ across registries (`voom`, `@goldziher/voom`, `voom-cli`) while the
  binary is `voom` everywhere.

[Unreleased]: https://github.com/Goldziher/voom/commits/main
