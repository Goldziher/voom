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
    /// Human label for `--list-caches`.
    pub name: &'static str,
    /// Locations relative to the user's home directory. Several when a tool's cache moved or
    /// differs by platform; every one of them must satisfy `markers` on its own.
    pub locations: &'static [&'static str],
    /// Paths that must appear *inside* the directory for it to be one, each relative to it and
    /// optionally behind a `*/` for "in any immediate subdirectory". Any one is enough, and an
    /// empty list is rejected at construction time.
    pub markers: &'static [&'static str],
    /// What removing it costs, printed by `--list-caches`. Always a re-download or a rebuild,
    /// never lost work — an entry that cannot say so does not belong here.
    pub note: &'static str,
    /// Whether a bare `--clean-caches` (or `all`) includes it. Off for a cache whose loss is
    /// expensive rather than merely slow — gigabytes that a metered or absent network cannot
    /// replace — so it is removed only by naming it.
    pub in_all: bool,
    /// Whether the tool writes the cache read-only by design, as Go does its module cache.
    /// Removal then restores the owner's bits inside it first — what the tool's own clean
    /// command does (`go clean -modcache`) — instead of failing part-way unless `--force`.
    pub read_only: bool,
}

impl Cache {
    /// Whether the directory holds one of this entry's markers.
    ///
    /// A marker is a plain relative path, checked with one `lstat`; a leading `*/` means "in any
    /// immediate subdirectory", for tools that write their bookkeeping into a version directory
    /// whose name they choose (`node-gyp/<node-version>/installVersion`). The wildcard costs one
    /// `read_dir` of the candidate and is affordable for the same reason [`Anchor::Inside`]
    /// resolution is: a cache location is reached at most once in a walk, and the walker prunes
    /// it rather than enumerating it. Nothing is allocated per entry and a non-UTF-8 name cannot
    /// be mistaken for a match.
    ///
    /// An unreadable directory answers `false`: nothing proved it, which is the safe direction.
    ///
    /// [`Anchor::Inside`]: crate::catalog::Anchor::Inside
    #[must_use]
    pub fn proven_in(&self, dir: &std::path::Path) -> bool {
        self.markers.iter().any(|marker| match marker.strip_prefix("*/") {
            Some(inner) => std::fs::read_dir(dir).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()) && exists(&entry.path().join(inner)))
            }),
            None => exists(&dir.join(marker)),
        })
    }
}

/// Whether anything, a dangling link included, is named by `path`.
fn exists(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
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
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "cargo-git",
        name: "Cargo git checkouts",
        locations: &[".cargo/git"],
        markers: &["CACHEDIR.TAG"],
        note: "Checkouts of git dependencies. Re-cloned on the next build; needs network.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "uv",
        name: "uv",
        locations: &[".cache/uv"],
        markers: &["CACHEDIR.TAG"],
        note: "Wheels and built distributions. `uv cache prune` removes only the stale part.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "gradle-caches",
        name: "Gradle caches",
        locations: &[".gradle/caches"],
        markers: &["CACHEDIR.TAG"],
        note: "Downloaded dependencies and build caches. The next build refetches them.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "go-build",
        name: "Go build cache",
        locations: &["Library/Caches/go-build", ".cache/go-build"],
        // `trim.txt` is go's own bookkeeping file, and the README beside it says outright to
        // run `go clean -cache` when the directory gets large.
        markers: &["trim.txt"],
        note: "Compiled package objects. Rebuilt locally, no network needed.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "go-mod",
        name: "Go module cache",
        locations: &["go/pkg/mod"],
        markers: &["cache"],
        note: "Downloaded modules. `go clean -modcache` is the same thing; needs network after.",
        in_all: true,
        read_only: true,
    },
    Cache {
        id: "npm",
        name: "npm cache",
        locations: &[".npm/_cacache"],
        markers: &["content-v2", "index-v5"],
        note: "Content-addressed package tarballs. Refetched on the next install.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "pnpm",
        name: "pnpm metadata cache",
        locations: &["Library/Caches/pnpm", ".cache/pnpm"],
        markers: &["lockfile-verified.jsonl"],
        note: "Registry metadata. The content-addressable store is `pnpm-store`.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "pub",
        name: "Dart pub cache",
        locations: &[".pub-cache"],
        markers: &["hosted-hashes"],
        note: "Downloaded packages. `dart pub get` refetches them; needs network.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "deno",
        name: "Deno cache",
        locations: &["Library/Caches/deno", ".cache/deno", ".deno"],
        markers: &["dep_analysis_cache_v2", "node_analysis_cache_v2"],
        note: "Fetched modules and analysis caches. Refetched on the next run; needs network.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "poly",
        name: "poly cache",
        locations: &[".cache/poly", "Library/Caches/poly", "AppData/Local/poly"],
        markers: &["CACHEDIR.TAG"],
        note: "Lint and format results, hook staged snapshots and fetched remote sources. \
               Rebuilt on the next run.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "alef-global",
        name: "alef global cache",
        locations: &[".cache/alef"],
        markers: &["zig-hashes.json"],
        note: "Shared snippet build output and toolchain hashes. Rebuilt on the next run.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "pnpm-store",
        name: "pnpm content-addressable store",
        locations: &[
            "Library/pnpm/store",
            ".local/share/pnpm/store",
            ".pnpm-store",
            "AppData/Local/pnpm/store",
        ],
        // The store keeps one directory per layout version, and each has a file only pnpm
        // writes: v10's `index`, v11's SQLite `index.db`, v3's `files`. The version directory
        // alone is not enough, which is why these are two segments deep.
        markers: &["v11/index.db", "v10/index", "v3/files"],
        note: "Every package pnpm has installed. Projects hold hard links into it, so their \
               `node_modules` survive; the next install of anything not already linked refetches \
               it, and needs network.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "pip",
        name: "pip cache",
        locations: &["Library/Caches/pip", ".cache/pip", "AppData/Local/pip/Cache"],
        markers: &["http-v2", "wheels", "selfcheck"],
        note: "Downloaded and locally built wheels. The next install refetches or rebuilds them.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "homebrew",
        name: "Homebrew downloads",
        locations: &["Library/Caches/Homebrew", ".cache/Homebrew"],
        // `.cleaned` is the stamp `brew cleanup` leaves; the command list is brew's own too.
        markers: &[".cleaned", "all_commands_list.txt"],
        note: "Bottles and API metadata already downloaded. `brew cleanup` removes only the \
               stale part; a reinstall or upgrade refetches the rest.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "prek",
        name: "prek cache",
        locations: &[".cache/prek"],
        markers: &["config-tracking.json", "prek.log"],
        note: "Cloned hook repositories and their environments. Rebuilt on the next run; needs network.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "pre-commit",
        name: "pre-commit cache",
        locations: &[".cache/pre-commit"],
        markers: &["db.db"],
        note: "Cloned hook repositories and their environments. Rebuilt on the next run; needs network.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "ms-playwright",
        name: "Playwright browsers",
        locations: &[
            "Library/Caches/ms-playwright",
            ".cache/ms-playwright",
            "AppData/Local/ms-playwright",
        ],
        markers: &[".links"],
        note: "Downloaded browser builds. Playwright tests fail until `playwright install` fetches \
               them again; needs network.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "node-gyp",
        name: "node-gyp headers",
        locations: &[
            "Library/Caches/node-gyp",
            ".cache/node-gyp",
            "AppData/Local/node-gyp/Cache",
        ],
        markers: &["*/installVersion"],
        note: "Node headers for native addon builds. Refetched by the next `node-gyp rebuild`; needs network.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "golangci-lint",
        name: "golangci-lint cache",
        locations: &[
            "Library/Caches/golangci-lint",
            ".cache/golangci-lint",
            "AppData/Local/golangci-lint",
        ],
        // The Go build cache's own bookkeeping file, which golangci-lint's cache shares.
        markers: &["trim.txt"],
        note: "Analysis results. The next lint run is slow once, then warm again; no network needed.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "tree-sitter-language-pack",
        name: "tree-sitter language pack",
        locations: &[
            "Library/Caches/tree-sitter-language-pack",
            ".cache/tree-sitter-language-pack",
        ],
        markers: &["*/.download.lock", "*/manifest.json"],
        note: "Downloaded parser bundles, one directory per release. Refetched on first use; needs network.",
        in_all: true,
        read_only: false,
    },
    Cache {
        id: "huggingface",
        name: "Hugging Face hub",
        locations: &[".cache/huggingface/hub"],
        // The lock directory `huggingface_hub` creates on its first download. Only `hub/` is
        // named: its parent also holds the login token, which is not regenerable.
        markers: &[".locks"],
        note: "Downloaded models and datasets, often gigabytes each, some gated or withdrawn. \
               Only removed when named; refetching needs network and may need a login.",
        in_all: false,
        read_only: false,
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

    /// The third set sharing that one column (ADR 0013). `tagged` must not be an ecosystem id or
    /// a cache id either, or a reader cannot tell which table produced the line.
    #[test]
    fn no_id_collides_with_tagged() {
        let tagged = crate::classify::TAGGED_ID;
        assert!(
            crate::catalog::find(tagged).is_none(),
            "`{tagged}` is also an ecosystem id, so the report cannot say which"
        );
        for cache in CACHES {
            assert_ne!(
                cache.id, tagged,
                "`{tagged}` is also a cache id, so the report cannot say which"
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

    /// Builds the marker as the tool would leave it: a `*/` marker goes one directory down,
    /// under a name standing in for the version or profile the tool chose.
    fn plant(dir: &std::path::Path, marker: &str) {
        let target = dir.join(marker.replace('*', "any-version"));
        std::fs::create_dir_all(target.parent().expect("a marker has a parent")).expect("a marker dir");
        std::fs::write(target, b"x").expect("a marker file");
    }

    /// The generated counterpart of the ecosystem harness: every marker of every entry proves
    /// its cache, and a directory holding something else does not.
    #[test]
    fn every_marker_proves_its_cache_and_nothing_else_does() {
        for cache in CACHES {
            for marker in cache.markers {
                let proven = tempfile::TempDir::new().expect("a temp dir");
                plant(proven.path(), marker);
                assert!(
                    cache.proven_in(proven.path()),
                    "`{}`: `{marker}` does not prove it",
                    cache.id
                );
            }
            let unproven = tempfile::TempDir::new().expect("a temp dir");
            std::fs::write(unproven.path().join("notes.txt"), b"x").expect("a file");
            assert!(
                !cache.proven_in(unproven.path()),
                "`{}` is proven by an unrelated file",
                cache.id
            );
        }
    }

    /// A marker is a relative path: an absolute or climbing one would let a cache be proven by
    /// something outside it.
    #[test]
    fn no_marker_escapes_the_cache_directory() {
        for cache in CACHES {
            for marker in cache.markers {
                let rest = marker.strip_prefix("*/").unwrap_or(marker);
                assert!(
                    std::path::Path::new(rest)
                        .components()
                        .all(|component| matches!(component, std::path::Component::Normal(_)))
                        && !rest.contains('*'),
                    "`{}`: `{marker}` is not a plain relative path, optionally behind `*/`",
                    cache.id
                );
            }
        }
    }

    /// A version directory alone proves nothing: `node-gyp` is proven by what node-gyp wrote
    /// inside one, not by there being a directory named `20.1.0`.
    #[test]
    fn should_not_prove_a_wildcard_marker_from_an_empty_subdirectory() {
        let node_gyp = CACHES
            .iter()
            .find(|cache| cache.id == "node-gyp")
            .expect("node-gyp is a cache");
        let dir = tempfile::TempDir::new().expect("a temp dir");
        std::fs::create_dir_all(dir.path().join("20.1.0")).expect("a version dir");

        assert!(!node_gyp.proven_in(dir.path()));
    }

    /// Gigabytes of models are not "a re-download" in the sense the others are, so a bare
    /// `--clean-caches` leaves them alone and only the id removes them.
    #[test]
    fn should_keep_expensive_caches_out_of_the_bare_flag() {
        let out: Vec<&str> = CACHES
            .iter()
            .filter(|cache| !cache.in_all)
            .map(|cache| cache.id)
            .collect();
        assert_eq!(out, vec!["huggingface"]);
    }

    #[test]
    fn should_know_the_machine_wide_stores_the_audit_found() {
        for id in [
            "pnpm-store",
            "pip",
            "homebrew",
            "prek",
            "pre-commit",
            "ms-playwright",
            "node-gyp",
            "golangci-lint",
            "huggingface",
            "tree-sitter-language-pack",
        ] {
            assert!(CACHES.iter().any(|cache| cache.id == id), "`{id}` is not in the table");
        }
    }
}
