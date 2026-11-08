#!/usr/bin/env python3
"""The two numbers a downloader is allowed to grow in.

    python3 packaging/tests/budgets.py target/release/tafas
    python3 packaging/tests/budgets.py target/release/tafas --write

A binary's size and the time it takes to print its own name are the two costs a
user feels before the program has done anything, and both of them creep. The
numbers are recorded in `packaging/budgets.json` and compared against, not merely
reported: a build that is bigger than the budget fails the job and says by how
much, so the answer to "why is this 40 MiB" is a commit and not an archaeology.

The tolerance is a percentage of the recorded number, with a floor on the clock:
ten percent of eight milliseconds is under a millisecond, which is noise on a
cold CI machine, so the start is also allowed to be fifteen milliseconds slower
in absolute terms. The size has no such floor — a byte is a byte.

`--write` records what was measured. It is meant for the commit that raises the
budget on purpose; the file it writes says where the numbers came from.
"""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import time
from pathlib import Path

BUDGET = Path(__file__).resolve().parents[1] / "budgets.json"

# How many times the binary is started before its start is believed: the first
# run of a freshly written file is the page cache's, not the program's.
STARTS = 7

# A start is allowed to be this much slower than the budget even when the
# percentage would allow less, because the clock on a shared runner is not a
# clock.
START_FLOOR_MILLIS = 15.0


def measure(binary: Path) -> dict:
    """The size in bytes and the median time to `--version`, in milliseconds."""
    binary_bytes = binary.stat().st_size
    starts: list[float] = []
    for _ in range(STARTS):
        begun = time.perf_counter()
        result = subprocess.run(
            [str(binary), "--version"], capture_output=True, check=False
        )
        starts.append((time.perf_counter() - begun) * 1000.0)
        if result.returncode != 0:
            raise SystemExit(
                f"{binary} --version exited {result.returncode}: "
                f"{result.stderr.decode('utf-8', 'replace').strip()}"
            )
    return {
        "binary_bytes": binary_bytes,
        "startup_millis": round(statistics.median(starts), 2),
    }


def limits(budget: dict) -> dict:
    """What the budget actually allows, after the tolerance."""
    tolerance = 1.0 + budget["tolerance_percent"] / 100.0
    return {
        "binary_bytes": budget["binary_bytes"] * tolerance,
        "startup_millis": max(
            budget["startup_millis"] * tolerance,
            budget["startup_millis"] + START_FLOOR_MILLIS,
        ),
    }


def compare(measured: dict, budget: dict) -> list[str]:
    """Every number that is over what the budget allows, as sentences."""
    over = []
    allowed = limits(budget)
    if measured["binary_bytes"] > allowed["binary_bytes"]:
        over.append(
            f"the binary is {measured['binary_bytes']} bytes, "
            f"the budget allows {budget['binary_bytes']} plus "
            f"{budget['tolerance_percent']}% "
            f"({int(allowed['binary_bytes'])}), "
            f"over by {measured['binary_bytes'] - int(allowed['binary_bytes'])}"
        )
    if measured["startup_millis"] > allowed["startup_millis"]:
        over.append(
            f"`--version` takes {measured['startup_millis']} ms, "
            f"the budget allows {budget['startup_millis']} plus "
            f"{budget['tolerance_percent']}% or {START_FLOOR_MILLIS:g} ms "
            f"({allowed['startup_millis']:.1f}), "
            f"over by {measured['startup_millis'] - allowed['startup_millis']:.1f} ms"
        )
    return over


def report(measured: dict, budget: dict | None) -> str:
    lines = [
        f"binary    {measured['binary_bytes']:>10} bytes",
        f"startup   {measured['startup_millis']:>10} ms (median of {STARTS})",
    ]
    if budget:
        allowed = limits(budget)
        lines += [
            f"budget    {budget['binary_bytes']:>10} bytes "
            f"(allows {int(allowed['binary_bytes'])})",
            f"budget    {budget['startup_millis']:>10} ms "
            f"(allows {allowed['startup_millis']:.1f})",
        ]
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("binary", type=Path, help="the release binary to measure")
    parser.add_argument(
        "--write",
        action="store_true",
        help="record what was measured instead of comparing against it",
    )
    args = parser.parse_args(argv)

    if not args.binary.is_file():
        raise SystemExit(f"no binary at {args.binary}: build it first")

    measured = measure(args.binary)
    budget = json.loads(BUDGET.read_text()) if BUDGET.is_file() else None

    if args.write:
        if budget is None:
            budget = {
                "measured_on": "ubuntu-latest, release, x86_64-unknown-linux-gnu",
                "tolerance_percent": 10,
            }
        budget.update(
            {
                "binary_bytes": measured["binary_bytes"],
                "startup_millis": round(measured["startup_millis"]),
            }
        )
        BUDGET.write_text(json.dumps(budget, indent=2) + "\n")
        print(report(measured, None))
        print(f"\nrecorded in {BUDGET}")
        return 0

    if budget is None or not budget.get("binary_bytes"):
        print(report(measured, None))
        print(
            "\nno budget is recorded yet: "
            f"run `python3 {sys.argv[0]} {args.binary} --write` and commit "
            f"{BUDGET.name}"
        )
        return 1

    print(report(measured, budget))
    over = compare(measured, budget)
    if not over:
        print("\nwithin the budget")
        return 0
    for line in over:
        print(f"\nover the budget: {line}")
    print(
        "\nif the growth is the point, raise the budget in a commit of its own: "
        f"`python3 {sys.argv[0]} {args.binary} --write`"
    )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
