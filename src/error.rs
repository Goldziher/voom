//! Typed library errors.
//!
//! `thiserror` here so callers of `voom::` can match on what failed; `anyhow` only at the
//! binary boundary in `main.rs`. Every variant that describes a filesystem operation names the
//! path, because an error that does not say which path it was about is not actionable in a
//! tool that walks millions of them.

use std::path::PathBuf;

/// Anything the library can fail at.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A pattern in the compiled-in catalog could not be compiled. This is a bug in voom, not
    /// in the user's tree — the catalog is `const` data and its patterns are covered by tests.
    #[error("the built-in catalog contains an invalid pattern `{pattern}`")]
    CatalogPattern {
        /// The offending pattern.
        pattern: String,
        /// Why it would not compile.
        #[source]
        source: globset::Error,
    },

    /// Watch mode could not start or could not continue.
    ///
    /// An `inotify` limit is reported here with the paths that could not be watched, rather
    /// than degrading silently into a watcher that sees nothing.
    #[error("watch mode failed: {reason}")]
    Watch {
        /// What went wrong.
        reason: String,
    },

    /// A configuration file could not be read, parsed, or validated.
    ///
    /// Always names the file: with several merged layers, an error that does not say which one
    /// was wrong is not actionable.
    #[error("in `{}`: {reason}", path.display())]
    Config {
        /// The file.
        path: PathBuf,
        /// What was wrong with it.
        reason: String,
    },

    /// A name given to `--ecosystem`, `--enable` or `--disable` that the catalog does not
    /// answer to.
    ///
    /// Rejected rather than ignored: a run that silently sweeps nothing looks exactly like a
    /// clean machine.
    #[error("`{name}` is not a known ecosystem or artifact — run `voom catalog` to see the table")]
    UnknownEcosystem {
        /// What was asked for.
        name: String,
    },

    /// A name given to `--clean-caches` that the cache table does not answer to.
    ///
    /// Rejected for the same reason as an unknown ecosystem: a mistyped id would otherwise
    /// remove nothing and report success, which is indistinguishable from an empty cache.
    #[error("`{id}` is not a known cache — run `voom caches` to see the table")]
    UnknownCache {
        /// What was asked for.
        id: String,
    },

    /// A duration string in configuration or on the command line would not parse.
    ///
    /// Never silently defaulted: a keep policy is a safety guard, and one the user thought they
    /// had set must not be quietly absent.
    #[error("`{input}` is not a duration: {reason}")]
    InvalidDuration {
        /// What was written.
        input: String,
        /// What was wrong with it.
        reason: &'static str,
    },

    /// A size string would not parse.
    #[error("`{input}` is not a size: {reason}")]
    InvalidSize {
        /// What was written.
        input: String,
        /// What was wrong with it.
        reason: &'static str,
    },

    /// The worker pool that sizes and removes artifacts could not be built.
    ///
    /// Fatal rather than a fallback to the default pool: `-j` exists to keep voom from
    /// saturating a spinning disk or a network filesystem, so quietly ignoring it would do the
    /// one thing the user asked it not to.
    #[error("building a worker pool of {jobs} threads")]
    ThreadPool {
        /// How many threads were asked for.
        jobs: usize,
        /// The underlying failure.
        #[source]
        source: rayon::ThreadPoolBuildError,
    },

    /// A directory could not be listed.
    #[error("reading directory `{}`", path.display())]
    ReadDir {
        /// The directory.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

/// The library's result type.
pub type Result<T, E = Error> = std::result::Result<T, E>;
