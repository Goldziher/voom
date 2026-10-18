//! Expands an explicit aggressive invocation through the ordinary selection mechanisms.

use super::PruneArgs;
use crate::catalog::CATALOG;

impl PruneArgs {
    /// The `--enable` specs, with `--clean-dependencies` expanded into the group it stands for.
    ///
    /// Sugar rather than a mechanism: expanding here means the dependency opt-in travels the
    /// same path as any other one — applied above every configuration layer, resolved against
    /// each artifact's own directory, and listed by `voom config show` as a departure from the
    /// catalog default.
    pub(super) fn enable_specs(&self) -> Vec<String> {
        let mut enable = self.enable.clone();
        if self.aggressive {
            enable.extend(CATALOG.iter().map(|ecosystem| ecosystem.id.to_owned()));
        }
        if self.clean_dependencies || self.aggressive {
            enable.extend(
                crate::catalog::DEPENDENCY_ARTIFACTS
                    .iter()
                    .map(|spec| (*spec).to_owned()),
            );
        }
        enable
    }

    pub(super) fn cache_specs(&self) -> Vec<String> {
        let mut caches = Vec::new();
        if self.aggressive {
            caches.push("all".to_owned());
            caches.extend(crate::caches::CACHES.iter().map(|cache| cache.id.to_owned()));
        }
        // Explicit negative ids retain their normal precedence over the aggressive defaults.
        caches.extend(self.clean_caches.iter().cloned());
        caches
    }
}
