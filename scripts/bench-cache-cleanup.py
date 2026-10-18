"""Compare cleanup binaries and native rm on disposable, allocated small-file trees.

Run through `task bench -- --before /path/to/baseline --after /path/to/candidate`.
Creation, copying, and validation are outside each timed subprocess. These are warm
filesystem benchmarks, including CLI startup, scan, sizing, and removal. They model
cache layouts through a marker-proven target/ without touching machine caches.
"""

import argparse
import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

FILE_BYTES = 4096
SHARDS = 256
PACKAGE_FILES = 12
DEEP_LEVELS = 16
BLOCK_BYTES = 512
ORDER_SEED = 41
LAYOUTS = ("flat", "sharded", "packages", "deep")


def positive_integer(value: str) -> int:
    """Reject empty benchmark matrices and invalid worker counts at the boundary."""
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return parsed


def executable(value: str) -> Path:
    """Resolve a binary now so changing subprocess working directory is harmless."""
    located = shutil.which(value)
    if located is None:
        raise argparse.ArgumentTypeError(f"executable not found: {value}")
    return Path(located).resolve()


def arguments() -> argparse.Namespace:
    """Define a benchmark that only creates and removes its own temporary data."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True, type=executable, help="baseline voom binary")
    parser.add_argument("--after", required=True, type=executable, help="candidate voom binary")
    parser.add_argument("--files", type=positive_integer, default=12000, help="files per layout (default: 12000)")
    parser.add_argument("--runs", type=positive_integer, default=3, help="repetitions per method (default: 3)")
    parser.add_argument("--jobs", "-j", type=positive_integer, help="voom workers; omit for voom's defaults")
    parser.add_argument("--layouts", nargs="+", choices=LAYOUTS, default=list(LAYOUTS))
    return parser.parse_args()


def file_parent(root: Path, layout: str, index: int) -> Path:
    """Reproduce the fanout shapes responsible for different cleanup costs."""
    if layout == "flat":
        return root
    if layout == "packages":
        return root / f"package-{index // PACKAGE_FILES:05d}" / "src" / "nested"
    if layout == "deep":
        root = root.joinpath(*(["prefix"] * DEEP_LEVELS))
    return root / f"{index % SHARDS:02x}"


def prepare_seed(root: Path, layout: str, files: int) -> None:
    """Write real data instead of sparse files or hardlinks that distort accounting."""
    payload = os.urandom(FILE_BYTES)
    root.mkdir()
    for index in range(files):
        parent = file_parent(root, layout, index)
        parent.mkdir(parents=True, exist_ok=True)
        (parent / f"{index:08x}").write_bytes(payload)


def allocated_bytes(root: Path) -> int:
    """Use voom's platform-specific measurement, including directory allocations."""
    total = 0
    pending = [root]
    while pending:
        path = pending.pop()
        metadata = path.lstat()
        total += metadata.st_blocks * BLOCK_BYTES if os.name == "posix" else metadata.st_size
        if path.is_dir():
            pending.extend(path.iterdir())
    return total


def command_for(method: str, args: argparse.Namespace, root: Path, native_rm: Path) -> list[str]:
    """Keep every deletion argument inside a freshly created fixture."""
    if method == "rm":
        return [str(native_rm), "-rf", "--", str(root / "target")]
    command = [str(getattr(args, method)), "--no-git", "--no-bazel", "--format", "json"]
    command.extend(["--config", str(root / "voom.toml")])
    if args.jobs is not None:
        command.extend(["-j", str(args.jobs)])
    return [*command, str(root)]


def validate_report(stdout: str, target: Path, expected: int) -> dict:
    """Fail the experiment if a fast run skipped work or reported inaccurate bytes."""
    report = json.loads(stdout)
    totals = report["totals"]
    failures = {key: totals[key] for key in ("failed", "partial", "refused", "unmeasured") if totals[key] != 0}
    if failures or report["walk_errors"]:
        raise RuntimeError(f"cleanup errors for {target}: {failures}, walk_errors={report['walk_errors']}")
    if totals["bytes"] != expected or totals["reclaimed"] != 1:
        raise RuntimeError(f"incorrect cleanup totals for {target}: expected 1 artifact/{expected} bytes, got {totals}")
    artifacts = report["artifacts"]
    if len(artifacts) != 1 or Path(artifacts[0]["path"]) != target:
        raise RuntimeError(f"unexpected artifact selection for {target}: {artifacts}")
    if artifacts[0]["outcome"]["status"] != "removed":
        raise RuntimeError(f"artifact was not removed for {target}: {artifacts[0]['outcome']}")
    return {"reported_bytes": totals["bytes"], "timings_us": report.get("timings_us"), "failures": failures}


def measure(method: str, args: argparse.Namespace, seed: Path, root: Path, native_rm: Path) -> dict:
    """Time only the process, validating its output and survivors afterward."""
    target = root / "target"
    shutil.copytree(seed, target)
    expected = allocated_bytes(target)
    marker = root / "Cargo.toml"
    marker.write_text('[package]\nname = "cleanup-benchmark"\nversion = "0.0.0"\n')
    (root / "voom.toml").write_text("")
    command = command_for(method, args, root, native_rm)
    start = time.perf_counter()
    completed = subprocess.run(command, capture_output=True, text=True, cwd=root, check=False)
    elapsed = time.perf_counter() - start
    if completed.returncode != 0:
        raise RuntimeError(f"{method} exited {completed.returncode}: {completed.stderr.strip()}")
    result = {"wall_seconds": elapsed, "files_per_second": args.files / elapsed, "expected_bytes": expected}
    if method != "rm":
        result.update(validate_report(completed.stdout, target, expected))
    if target.exists() or target.is_symlink():
        raise RuntimeError(f"{method} left its fixture behind: {target}")
    if not marker.is_file():
        raise RuntimeError(f"{method} removed the marker outside its artifact: {marker}")
    return result


def summarize(samples: list[dict], files: int) -> dict:
    """Expose raw runs alongside aggregation so variance remains visible."""
    durations = [sample["wall_seconds"] for sample in samples]
    median = statistics.median(durations)
    return {
        "wall_seconds_mean": statistics.mean(durations),
        "wall_seconds_median": median,
        "files_per_second_median": files / median,
        "samples": samples,
    }


def benchmark_layout(args: argparse.Namespace, layout: str, base: Path, native_rm: Path) -> dict:
    """Reuse one seed, randomizing command order without timing fixture setup."""
    seed = base / "seed"
    prepare_seed(seed, layout, args.files)
    samples = {method: [] for method in ("before", "after", "rm")}
    order_generator = random.Random(ORDER_SEED)
    execution_order = []
    for run in range(args.runs):
        methods = list(samples)
        order_generator.shuffle(methods)
        execution_order.append(methods)
        for method in methods:
            root = base / f"run-{run}-{method}"
            root.mkdir()
            sample = measure(method, args, seed, root, native_rm)
            samples[method].append(sample)
            print(f"{layout}: run {run + 1}/{args.runs} {method}: {sample['wall_seconds']:.3f}s", file=sys.stderr)
    return {
        "execution_order": execution_order,
        "methods": {method: summarize(runs, args.files) for method, runs in samples.items()},
    }


def main() -> int:
    """Emit one JSON document suitable for retaining with benchmark results."""
    args = arguments()
    native_rm = executable("rm")
    result = {
        "files_per_layout": args.files,
        "file_bytes": FILE_BYTES,
        "runs": args.runs,
        "jobs": args.jobs,
        "before": str(args.before),
        "after": str(args.after),
        "native_rm": str(native_rm),
        "order_seed": ORDER_SEED,
        "layouts": {},
    }
    try:
        with tempfile.TemporaryDirectory(prefix="voom-cache-benchmark-") as temporary:
            for index, layout in enumerate(args.layouts):
                base = Path(temporary).resolve() / f"{index}-{layout}"
                base.mkdir()
                result["layouts"][layout] = benchmark_layout(args, layout, base, native_rm)
    except (OSError, RuntimeError, ValueError, KeyError, TypeError) as error:
        print(f"cache cleanup benchmark failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
