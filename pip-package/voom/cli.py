from __future__ import annotations

import sys

from .downloader import run_voom


def main() -> None:
    run_voom(sys.argv[1:])
