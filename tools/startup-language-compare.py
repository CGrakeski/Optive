"""Measure cold-process CLI latency for Optive and CPython.

Build Optive first with: cargo build --release --bin Optive
An explicit executable path may be passed as the first argument.
"""

from __future__ import annotations

import statistics
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SAMPLES = 20
WARMUPS = 3


def find_optive() -> Path:
    if len(sys.argv) > 1:
        candidates = [Path(sys.argv[1])]
    else:
        candidates = [
            ROOT / "target" / "release" / "Optive.exe",
            ROOT / "target-criterion-codex" / "release" / "Optive.exe",
        ]
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    raise SystemExit(
        "Optive.exe not found; run `cargo build --release --bin Optive` "
        "or pass its path as the first argument"
    )


def run(command: list[str]) -> None:
    subprocess.run(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=True,
    )


def measure(command: list[str]) -> list[float]:
    for _ in range(WARMUPS):
        run(command)
    samples = []
    for _ in range(SAMPLES):
        start = time.perf_counter_ns()
        run(command)
        samples.append((time.perf_counter_ns() - start) / 1_000_000)
    return samples


def percentile_90(values: list[float]) -> float:
    return sorted(values)[int((len(values) - 1) * 0.9)]


def main() -> None:
    optive = find_optive()
    commands = {
        "Optive -c none": [str(optive), "-c", "none"],
        "Python -c None": [sys.executable, "-c", "None"],
    }
    print(f"Optive executable: {optive}")
    print(f"Samples: {SAMPLES} (+ {WARMUPS} warmups)\n")
    for name, command in commands.items():
        samples = measure(command)
        print(
            f"{name:18} median {statistics.median(samples):8.3f} ms  "
            f"min {min(samples):8.3f} ms  p90 {percentile_90(samples):8.3f} ms"
        )


if __name__ == "__main__":
    main()
