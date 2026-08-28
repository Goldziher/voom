//! Hierarchical TOML configuration (ADR 0004).
//!
//! Merged from lowest to highest precedence:
//!
//! 1. Built-in defaults — the catalog's own `default_on` flags.
//! 2. User config — `${XDG_CONFIG_HOME:-~/.config}/voom/config.toml`.
//! 3. Every `voom.toml` from the scan root down to each candidate, nearest wins.
//! 4. `--config <path>`, which replaces layers 2 and 3 entirely.
//! 5. Command-line flags.
//!
//! Layer 3 is what makes the motivating case work: a repository commits a `voom.toml` that
//! protects its own quirks, and it keeps working when someone sweeps `$HOME` from above.

pub mod load;
pub mod resolve;
pub mod schema;

pub use load::{Layer, Sources, discover, user_config_path};
pub use resolve::{Resolved, Resolver};
