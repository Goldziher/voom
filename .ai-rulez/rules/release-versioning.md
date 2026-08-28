---
priority: high
---

# Release and versioning — five surfaces, one number

voom ships through five channels from one binary (ADR 0010). The version appears in **five
files** and they must agree exactly, or the release workflow fails its consistency gate
before publishing anything.

| Surface | File |
| --- | --- |
| crate | `Cargo.toml` |
| npm | `npm-package/package.json` |
| PyPI | `pip-package/pyproject.toml` |
| PyPI | `pip-package/voom/__init__.py` (`__version__`) |
| hooks | `poly-hooks.toml` (pinned in every `run` and `install` string) |

## Bumping

Always use the script. Never hand-edit one of the five:

```bash
scripts/bump-version.sh 0.2.0
git add -A && git commit -m "chore(release): v0.2.0"
git tag v0.2.0 && git push origin main v0.2.0
```

`poly-hooks.toml` is the one people forget. It pins an exact version in each channel's `run`
and `install` string (ADR 0009), so a missed bump ships a release whose hooks still install
the previous binary. The script rewrites every occurrence; CI checks it independently.

## Package names are asymmetric — do not "fix" them

| Channel | Name |
| --- | --- |
| crates.io | `voom` |
| npm | `@goldziher/voom` |
| PyPI | `voom-cli` |
| Homebrew | `voom` (in `Goldziher/homebrew-tap`) |
| binary on `$PATH` | `voom` |

They differ because `voom` was taken on npm and PyPI, and npm additionally rejected
`voom-cli` as too similar to an abandoned `voomcli`. ADR 0010 records the whole thing. This
is a scar, not an inconsistency to tidy up.

## The workflow filename is load-bearing

`.github/workflows/publish.yaml` is bound by name to the OIDC trusted-publisher configuration
on crates.io, npm, and PyPI. **Renaming it silently breaks publishing on all three at once.**
Do not rename it, and do not "normalize" the `.yaml` extension to `.yml`.

## Release candidates

Tag `vX.Y.Z-rc.N` for git and npm; PyPI normalizes it to `X.Y.ZrcN`. The workflow converts
between the two forms in both directions — CI when publishing, and the Python downloader when
resolving the release tag to fetch. Homebrew is skipped for pre-releases.

Related: [[tdd-and-hooks]].
