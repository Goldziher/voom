#!/usr/bin/env bash
# Rewrite the version across every surface that carries it.
#
#   ./scripts/bump-version.sh 0.2.0
#   ./scripts/bump-version.sh 0.2.0-rc.1
#
# Five files must agree or the release workflow fails its consistency gate before
# publishing anything (see adrs/0010-distribution-and-naming.md):
#
#   Cargo.toml                      X.Y.Z
#   npm-package/package.json        X.Y.Z
#   pip-package/pyproject.toml      X.Y.Z  (PyPI-normalized: 0.2.0-rc.1 -> 0.2.0rc1)
#   pip-package/voom/__init__.py    X.Y.Z  (PyPI-normalized)
#   poly-hooks.toml                 X.Y.Z  pinned in every run/install string
#
# poly-hooks.toml is the one that gets forgotten: a missed bump there ships a release
# whose git hooks still install the previous binary.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <version>   e.g. 0.2.0 or 0.2.0-rc.1" >&2
  exit 2
fi

VERSION="$1"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]]; then
  echo "not a valid version: $VERSION (expected X.Y.Z or X.Y.Z-rc.N)" >&2
  exit 1
fi

# PyPI normalizes 0.2.0-rc.1 to 0.2.0rc1 (PEP 440). pip-package/voom/downloader.py
# reverses this when resolving the release tag, so the two must stay in step.
# Only -rc.N pre-releases are supported; beta/alpha would normalize to b/a and the
# reverse mapping in the downloader does not handle them.
PY_VERSION="${VERSION/-rc./rc}"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

OLD_VERSION="$(grep -E '^version = "' Cargo.toml | head -1 | cut -d'"' -f2)"
echo "bumping ${OLD_VERSION} -> ${VERSION} (PyPI: ${PY_VERSION})"

# 1. Cargo.toml — only the [package] version, which is the first `version = ` line.
perl -0pi -e "s/^version = \"[^\"]*\"/version = \"${VERSION}\"/m unless \$done++" Cargo.toml

# 2. npm-package/package.json
perl -0pi -e "s/(\"version\":\s*)\"[^\"]*\"/\${1}\"${VERSION}\"/ unless \$done++" npm-package/package.json

# 3. pip-package/pyproject.toml
perl -0pi -e "s/^version = \"[^\"]*\"/version = \"${PY_VERSION}\"/m unless \$done++" pip-package/pyproject.toml

# 4. pip-package/voom/__init__.py
perl -pi -e "s/^__version__ = \"[^\"]*\"/__version__ = \"${PY_VERSION}\"/" pip-package/voom/__init__.py

# 5. poly-hooks.toml — every pinned channel string.
perl -pi -e "s{\@goldziher/voom\@[0-9][^ \"]*}{\@goldziher/voom\@${VERSION}}g; s{voom-cli==[0-9][^ \"]*}{voom-cli==${VERSION}}g" poly-hooks.toml

# Refresh the lock file so Cargo.lock records the new version.
cargo update -p voom --quiet 2>/dev/null || cargo update --quiet

echo
echo "updated:"
grep -H -m1 -E '^version = "' Cargo.toml pip-package/pyproject.toml
grep -H -m1 '"version"' npm-package/package.json
grep -H -m1 '__version__' pip-package/voom/__init__.py
grep -H -oE '(@goldziher/voom@|voom-cli==)[0-9][^ "]*' poly-hooks.toml | sort -u

echo
echo "next:"
echo "  git add -A && git commit -m \"chore(release): v${VERSION}\""
echo "  git tag v${VERSION} && git push origin main v${VERSION}"
