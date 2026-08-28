# voom-cli

Fast, safe, parallel build-artifact pruning across 24 language ecosystems.

```bash
pip install voom-cli
voom --dry-run ~
```

Or without installing:

```bash
uvx --from voom-cli voom --dry-run ~
```

The first run downloads the prebuilt `voom` binary for your platform from the
[GitHub release](https://github.com/Goldziher/voom/releases) matching this package's version
and caches it under `~/.cache/voom/`. Set `VOOM_BINARY` to point at your own build instead.

**voom deletes by default.** Use `--dry-run` to preview. It never removes a directory without
a marker file proving its ecosystem, and it never touches `node_modules/`.

Full documentation: <https://github.com/Goldziher/voom>
