# Configuration

The full `voom.toml` schema, its precedence rules, and the per-path overrides. `tests/docs.rs`
loads every example here through the real configuration pipeline, so an example that does not
work fails the build.

Back to the [README](../README.md).

## The file

voom merges configuration from built-in defaults, `~/.config/voom/config.toml`, and every
`voom.toml` from the scan root down to each candidate — nearest wins. A repository can therefore
protect its own quirks and still be swept correctly from `$HOME`. Every table in the schema
rejects unknown fields, so a typo in a key — `excludee`, `min_agee` — is a hard load error rather
than a guard that silently never took effect.

```toml
# Top-level keys come first: TOML would otherwise read them as part of the table above.
exclude = ["~/work/client-x/**", "**/fixtures/**"]   # never scanned, always wins
include = ["~/scratch/build-junk"]                   # swept without a marker — explicit intent

[ecosystems]
enable  = ["python.venv"]     # opt into a † artifact
disable = ["terraform"]

[git]
enabled = false      # skip the housekeeping a sweep otherwise does here

[caches]
enable = ["uv"]      # opt into a named tool cache — additive across the hierarchy, never subtractive

[keep]
min_age  = "7d"      # never remove something modified more recently
min_size = "1MB"     # not worth the rebuild below this
max_size = "50GB"    # probably not what you meant

[keep.rust]
min_age = "1d"

[[paths]]
match = "~/work/**"
keep  = { min_age = "30d" }

[[paths]]
match      = "~/experiments/**"
ecosystems = { enable = ["node.build"] }
```

Paths in `include` are swept without a marker proving them — the one place voom will remove
something it cannot prove. They report as `unanchored` rather than as an ecosystem, and JSON marks
them `"source": "config-include"`. `exclude` always wins over `include`, and `include` only
matches paths the walk actually visits: it cannot reach outside the roots you named, and it
cannot reach *inside* a directory the walk skips whole. For a dependency directory prefer
`--clean-dependencies` or `--enable`, which still prove the marker.

**On Windows, write paths in single quotes.** TOML basic strings take escapes, so
`exclude = ["C:\Users\me\work\**"]` fails to parse — `\U` starts a unicode escape. Use a TOML
literal string instead, which takes none:

```toml
exclude = ['C:\Users\me\work\**']
```

`voom config show [PATH]` lists the files the configuration was merged from, in ascending
precedence, and then the merged result. Details in [ADR 0004](../adrs/0004-configuration.md).
