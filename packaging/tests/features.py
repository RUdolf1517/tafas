#!/usr/bin/env python3
"""What a build without a site actually contains.

    python3 packaging/tests/features.py <tafas-for-one-site> [<full-tafas>]

`--no-default-features --features youtube` is a promise about the *binary*: a
build like that must not contain another site's code at all, not a route that
refuses. Three things follow from it, and each is checked here rather than
believed:

1. `--capabilities` lists exactly the sites that were compiled in.
2. An unsupported host is refused with the names this build has, and the error
   text does not mention a site that was compiled out.
3. The binary is smaller than the full build — the point of the feature is a
   smaller binary, and a feature that saves nothing is a feature that does not
   work.

The two binaries are arguments, so this script has no opinion about where
cargo put them. It exits non-zero with a sentence for the first thing that is
wrong.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


def run(binary: str, *arguments: str) -> subprocess.CompletedProcess:
    return subprocess.run([binary, *arguments], capture_output=True, text=True)


def capabilities(binary: str) -> dict:
    result = run(binary, "--capabilities")
    if result.returncode != 0:
        raise SystemExit(f"{binary} --capabilities exited {result.returncode}: {result.stderr.strip()}")
    return json.loads(result.stdout)


def main(argv: list[str]) -> int:
    if not argv:
        raise SystemExit(__doc__.strip().splitlines()[2].strip())

    one = argv[0]
    sites = capabilities(one).get("sites", [])
    if sites != ["youtube"]:
        raise SystemExit(f"--capabilities of a youtube-only build lists {sites}, expected ['youtube']")

    refusal = run(one, "https://vimeo.com/12345")
    text = (refusal.stderr + refusal.stdout).lower()
    if "no extractor" not in text:
        raise SystemExit(f"a site compiled out is not a named refusal: {text!r}")
    # The URL appears in the message, so the check is on the sentence after it:
    # "this build reads youtube" and nothing else.
    sentence = text.split("this build reads", 1)[-1]
    if "vimeo" in sentence or "twitch" in sentence or "reddit" in sentence:
        raise SystemExit(f"a refusal names a site this build does not have: {sentence!r}")
    if "youtube" not in sentence:
        raise SystemExit(f"a refusal must name the sites this build has: {sentence!r}")
    if refusal.returncode == 0:
        raise SystemExit("a site compiled out is a failure, not a skip: exit code 0")

    if len(argv) > 1:
        full = argv[1]
        small = Path(one).stat().st_size
        large = Path(full).stat().st_size
        if small >= large:
            raise SystemExit(
                f"the one-site binary is {small} bytes and the full one is {large}: "
                "the features save nothing"
            )
        print(f"features: one site {small} bytes, all sites {large} bytes, saved {large - small}")

    print(f"features: {one} reads {', '.join(sites)} and refuses the rest by name")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
