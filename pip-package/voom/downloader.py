from __future__ import annotations

import os
import platform
import ssl
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path
from urllib.error import URLError
from urllib.request import Request, urlopen

import certifi

REPO = "Goldziher/voom"
BINARY = "voom"


def _platform_triple() -> str:
    system = platform.system().lower()
    machine = platform.machine().lower()

    if system == "windows":
        if machine in {"amd64", "x86_64"}:
            return "x86_64-pc-windows-gnu"
        if machine in {"x86", "i386", "i686"}:
            raise RuntimeError("32-bit Windows is not supported")
    elif system == "linux":
        if machine in {"amd64", "x86_64"}:
            return "x86_64-unknown-linux-gnu"
        if machine in {"aarch64", "arm64"}:
            return "aarch64-unknown-linux-gnu"
    elif system == "darwin":
        if machine in {"amd64", "x86_64"}:
            return "x86_64-apple-darwin"
        if machine in {"aarch64", "arm64"}:
            return "aarch64-apple-darwin"

    raise RuntimeError(f"Unsupported platform: {system} {machine}")


def _python_version_to_tag(version: str) -> str:
    """Reverse PyPI's version normalization to recover the git tag.

    PyPI stores ``0.2.0rc1`` where the git tag is ``v0.2.0-rc.1``. The release
    workflow performs the forward conversion; this is the inverse, and the two
    must stay in step or release-candidate installs will 404.
    """
    if "rc" in version:
        core, suffix = version.split("rc")
        return f"{core}-rc.{suffix}"
    return version


def _asset(version: str) -> tuple[str, str]:
    tag = _python_version_to_tag(version)
    triple = _platform_triple()
    ext = "zip" if "windows" in triple else "tar.gz"
    url = f"https://github.com/{REPO}/releases/download/v{tag}/{BINARY}-{triple}.{ext}"
    return url, ext


def _download(url: str, destination: Path) -> None:
    request = Request(url, headers={"User-Agent": "voom-python-wrapper"})
    context = ssl.create_default_context(cafile=certifi.where())
    try:
        with urlopen(request, timeout=30, context=context) as response:
            if response.status != 200:
                raise RuntimeError(f"HTTP {response.status}: {response.reason}")
            destination.write_bytes(response.read())
    except URLError as exc:
        raise RuntimeError(f"Failed to download binary: {exc}") from exc


def _extract(archive: Path, ext: str, destination: Path) -> None:
    names = (BINARY, f"{BINARY}.exe")
    if ext == "zip":
        with zipfile.ZipFile(archive) as zf:
            for name in zf.namelist():
                if name.endswith(names):
                    with zf.open(name) as src, destination.open("wb") as dst:
                        dst.write(src.read())
                    return
    else:
        with tarfile.open(archive, "r:gz") as tar:
            for member in tar.getmembers():
                if member.name.endswith(names):
                    extracted = tar.extractfile(member)
                    if extracted is None:
                        continue
                    with extracted as src, destination.open("wb") as dst:
                        dst.write(src.read())
                    return
    raise RuntimeError("Binary not found in downloaded archive")


def _cache_root() -> Path:
    """Where the downloaded binary lives, per user and per version.

    Per user rather than beside the installed package: a wheel often lands somewhere the
    running user cannot write, which is exactly when the first run needs to write. The npm
    wrapper caches in the same place for the same reason (npm-package/download.js).
    """
    if platform.system().lower() == "windows":
        local_app_data = os.getenv("LOCALAPPDATA")
        if local_app_data:
            return Path(local_app_data) / BINARY
    return Path.home() / ".cache" / BINARY


def _cache_path(version: str) -> Path:
    cache_dir = _cache_root() / version
    cache_dir.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if platform.system().lower() == "windows" else ""
    return cache_dir / f"{BINARY}{suffix}"


def ensure_binary() -> str:
    """Return a path to the voom binary, downloading it on first use."""
    from . import __version__

    override = os.getenv("VOOM_BINARY")
    if override:
        return override

    binary_path = _cache_path(__version__)
    if binary_path.exists() and os.access(binary_path, os.X_OK):
        return str(binary_path)

    url, ext = _asset(__version__)
    print(f"Downloading voom binary v{__version__}...", file=sys.stderr)

    with tempfile.TemporaryDirectory() as tmpdir:
        archive_path = Path(tmpdir) / f"{BINARY}.{ext}"
        _download(url, archive_path)
        _extract(archive_path, ext, binary_path)

    if platform.system().lower() != "windows":
        binary_path.chmod(0o755)

    print("Binary downloaded successfully.", file=sys.stderr)
    return str(binary_path)


def run_voom(args: list[str]) -> None:
    """Run the voom binary with the given arguments and exit with its status."""
    binary_path = ensure_binary()

    try:
        result = subprocess.run([binary_path, *args], check=False)
    except FileNotFoundError as exc:
        raise RuntimeError(f"Binary not found at {binary_path}") from exc

    sys.exit(result.returncode)
