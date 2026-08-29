//! Named tool caches that `--clean-caches` can remove.
//!
//! [`CACHE_DIRS`](super::CACHE_DIRS) says where a walk must not go. This table says which of
//! those places can be *emptied*, and on what evidence. The two are deliberately separate: the
//! first is a rule about wandering, the second is a rule about proof.
//!
//! # Why a second table rather than catalog entries
//!
//! A cache cannot be classified the way `target/` is. Nothing beside `~/.cargo/registry` says
//! what it is — its siblings under `~/.cargo` are unrelated — and `~/.cache/uv` sits next to
//! `zig`, `huggingface` and whatever else the machine has. There is no marker to anchor on and
//! there never will be. The name is no help either: `registry`, `caches`, `mod` and `_cacache`
//! are not names voom could match anywhere on a disk without matching a great deal it must not
//! touch.
//!
//! What identifies a cache is the pair: a **location** that names the tool, and a **marker the
//! tool wrote inside it**. `CACHEDIR.TAG`, whose
//! [specification](https://bford.info/cachedir/) says the directory holds data the application
//! can regenerate, is the cross-tool spelling of exactly this and is already present in four of
//! the entries below. The rest use a file or directory that only that tool creates.
//!
//! # The bar for an entry
//!
//! Both halves must be unambiguous, and the marker must carry weight the location does not. A
//! cache whose only contents are shard directories named `0`–`f` or `a`–`z` fails that: the
//! marker proves nothing the path did not already say, so there is no evidence to weigh and the
//! entry is not shipped. See ADR 0012 for the ones this excludes and how much they hold.
//!
//! Nothing here is ever on by default, and nothing here is reachable without naming it.

/// One removable tool cache: where it lives, and what proves it is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cache {
    /// The id `--clean-caches` and `voom.toml` name it by.
    pub id: &'static str,
    /// Human label for `voom caches`.
    pub name: &'static str,
    /// Locations relative to the user's home directory. Several when a tool's cache moved or
    /// differs by platform; every one of them must satisfy `markers` on its own.
    pub locations: &'static [&'static str],
    /// Names that must appear *inside* the directory for it to be one. Any one is enough, and
    /// an empty list is rejected at construction time.
    pub markers: &'static [&'static str],
    /// What removing it costs, printed by `voom caches`. Always a re-download or a rebuild,
    /// never lost work — an entry that cannot say so does not belong here.
    pub note: &'static str,
}

impl Cache {
    /// Whether the directory holds one of this entry's markers.
    ///
    /// One `read_dir` of the candidate — the same shape as [`Anchor::Inside`] resolution and
    /// affordable for the same reason: a cache location is reached at most once in a walk, and
    /// the walker prunes it rather than enumerating it. Names are compared as `OsStr` so no
    /// allocation happens and a non-UTF-8 entry cannot be mistaken for a match.
    ///
    /// An unreadable directory answers `false`: nothing proved it, which is the safe direction.
    ///
    /// [`Anchor::Inside`]: crate::catalog::Anchor::Inside
    #[must_use]
    pub fn proven_in(&self, dir: &std::path::Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        entries.flatten().any(|entry| {
            let name = entry.file_name();
            self.markers
                .iter()
                .any(|marker| name.as_os_str() == std::ffi::OsStr::new(marker))
        })
    }
}

/// Every cache `--clean-caches` knows about.
///
/// Ids are stable: they appear in `voom.toml` and in `--clean-caches`, so one may be added but
/// not renamed. Sizes in the comments are what one workstation held on 2026-08-29, recorded so
/// a later reader can tell a big entry from a rounding error.
pub static CACHES: &[Cache] = &[
    Cache {
        id: "cargo-registry",
        name: "Cargo registry",
        locations: &[".cargo/registry"],
        markers: &["CACHEDIR.TAG"],
        note: "Crate sources and their index. Re-downloaded on the next build; needs network.",
    },
    Cache {
        id: "cargo-git",
        name: "Cargo git checkouts",
        locations: &[".cargo/git"],
        markers: &["CACHEDIR.TAG"],
        note: "Checkouts of git dependencies. Re-cloned on the next build; needs network.",
    },
    Cache {
        id: "uv",
        name: "uv",
        locations: &[".cache/uv"],
        markers: &["CACHEDIR.TAG"],
        note: "Wheels and built distributions. `uv cache prune` removes only the stale part.",
    },
    Cache {
        id: "gradle-caches",
        name: "Gradle caches",
        locations: &[".gradle/caches"],
        markers: &["CACHEDIR.TAG"],
        note: "Downloaded dependencies and build caches. The next build refetches them.",
    },
    Cache {
        id: "go-build",
        name: "Go build cache",
        locations: &["Library/Caches/go-build", ".cache/go-build"],
        // `trim.txt` is go's own bookkeeping file, and the README beside it says outright to
        // run `go clean -cache` when the directory gets large.
        markers: &["trim.txt"],
        note: "Compiled package objects. Rebuilt locally, no network needed.",
    },
    Cache {
        id: "go-mod",
        name: "Go module cache",
        locations: &["go/pkg/mod"],
        markers: &["cache"],
        note: "Downloaded modules. `go clean -modcache` is the same thing; needs network after.",
    },
    Cache {
        id: "npm",
        name: "npm cache",
        locations: &[".npm/_cacache"],
        markers: &["content-v2", "index-v5"],
        note: "Content-addressed package tarballs. Refetched on the next install.",
    },
    Cache {
        id: "pnpm",
        name: "pnpm metadata cache",
        locations: &["Library/Caches/pnpm", ".cache/pnpm"],
        markers: &["lockfile-verified.jsonl"],
        note: "Registry metadata. Not the content-addressable store, which holds linked packages.",
    },
    Cache {
        id: "pub",
        name: "Dart pub cache",
        locations: &[".pub-cache"],
        markers: &["hosted-hashes"],
        note: "Downloaded packages. `dart pub get` refetches them; needs network.",
    },
    Cache {
        id: "deno",
        name: "Deno cache",
        locations: &["Library/Caches/deno", ".cache/deno", ".deno"],
        markers: &["dep_analysis_cache_v2", "node_analysis_cache_v2"],
        note: "Fetched modules and analysis caches. Refetched on the next run; needs network.",
    },
    Cache {
        id: "alef-global",
        name: "alef global cache",
        locations: &[".cache/alef"],
        markers: &["zig-hashes.json"],
        note: "Shared snippet build output and toolchain hashes. Rebuilt on the next run.",
    },
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// The same rule the ecosystem catalog enforces: a location alone never justifies removal.
    #[test]
    fn every_cache_declares_a_marker() {
        for cache in CACHES {
            assert!(!cache.markers.is_empty(), "`{}` has no marker to prove it", cache.id);
            for marker in cache.markers {
                assert!(!marker.is_empty(), "`{}` has an empty marker", cache.id);
            }
        }
    }

    #[test]
    fn every_cache_declares_a_location_and_a_note() {
        for cache in CACHES {
            assert!(!cache.locations.is_empty(), "`{}` has no location", cache.id);
            assert!(!cache.note.is_empty(), "`{}` has no note", cache.id);
        }
    }

    /// Ids reach users through `--clean-caches` and `voom.toml`, so a duplicate would make one
    /// of the two unreachable and a rename would break a committed config.
    #[test]
    fn cache_ids_are_unique() {
        let mut seen = HashSet::new();
        for cache in CACHES {
            assert!(seen.insert(cache.id), "`{}` is declared twice", cache.id);
        }
    }

    /// The two tables share one column in the report and one word in conversation, so an id in
    /// both would leave a reader unable to tell which thing a line is about — `alef` names both
    /// an in-project cache and a machine-global one, and the second is `alef-global` for
    /// exactly this reason.
    #[test]
    fn no_cache_id_collides_with_an_ecosystem_id() {
        for cache in CACHES {
            assert!(
                crate::catalog::find(cache.id).is_none(),
                "`{}` is both a cache id and an ecosystem id, so the report cannot say which",
                cache.id
            );
        }
    }

    /// A location is joined onto the home directory, so an absolute or climbing one would
    /// escape it — the cache equivalent of the catalog's own containment invariant.
    #[test]
    fn no_cache_location_escapes_the_home_directory() {
        for cache in CACHES {
            for location in cache.locations {
                let path = std::path::Path::new(location);
                assert!(path.is_relative(), "`{}`: `{location}` is absolute", cache.id);
                assert!(
                    path.components()
                        .all(|component| matches!(component, std::path::Component::Normal(_))),
                    "`{}`: `{location}` has a `.` or `..` segment",
                    cache.id
                );
            }
        }
    }

    /// Every location must also be somewhere the walker already refuses to wander into, or
    /// enabling a cache would be the only thing standing between an ordinary sweep and it.
    #[test]
    fn every_cache_location_is_also_skipped_by_location() {
        for cache in CACHES {
            for location in cache.locations {
                assert!(
                    super::super::CACHE_DIRS
                        .iter()
                        .any(|skipped| location == skipped || location.starts_with(&format!("{skipped}/"))),
                    "`{}`: `{location}` is not under any CACHE_DIRS entry, so a default sweep \
                     would walk into it",
                    cache.id
                );
            }
        }
    }
}
