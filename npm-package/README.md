# @goldziher/voom

Fast, safe, parallel build-artifact pruning across 24 language ecosystems.

```bash
npm install -g @goldziher/voom
voom --dry-run ~
```

Installing downloads the prebuilt `voom` binary for your platform from the
[GitHub release](https://github.com/Goldziher/voom/releases) matching this package's version.

**voom deletes by default.** Use `--dry-run` to preview. It never removes a directory without
a marker file proving its ecosystem, and it never touches `node_modules/`.

Full documentation: <https://github.com/Goldziher/voom>
