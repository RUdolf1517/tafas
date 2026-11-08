#!/usr/bin/env python3
"""An SPDX document for what is in the archive.

    python3 packaging/sbom.py --version 0.4.0 --output dist/tafas-release-0.4.0.spdx.json

`--version` is the release tag without its `v`: the SBOM describes the release,
which is the thing a reader downloaded, and `Cargo.toml` may still say the last
released version while a release is being cut.

The archive answers "what is this"; the SBOM answers "what is in it", which is
the question a scanner, a packager or a licence review actually asks. It is built
from `Cargo.lock`, so it lists exactly the crates the build was locked to — not
what a later `cargo update` would have picked.

No network, no `cargo metadata`, no dependencies: the lock file is a small TOML
document and only four of its fields matter. The document is deterministic — the
packages are sorted, and the timestamp comes from `SOURCE_DATE_EPOCH` when the
build sets it — so two releases of one commit produce one SBOM.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# `[[package]]` blocks, then the four fields that identify a crate.
BLOCK = re.compile(r"^\[\[package\]\]$", re.MULTILINE)
FIELD = re.compile(r'^(name|version|source|checksum) = "([^"]*)"$', re.MULTILINE)
# SPDX ids are `[A-Za-z0-9.-]`; a crate name is nearly that already.
UNSAFE = re.compile(r"[^A-Za-z0-9.-]")


def read_packages(lock: str) -> list[dict[str, str]]:
    packages = []
    for block in BLOCK.split(lock)[1:]:
        fields = dict(FIELD.findall(block))
        if "name" in fields and "version" in fields:
            packages.append(fields)
    return packages


def spdx_id(name: str, version: str) -> str:
    return f"SPDXRef-Package-{UNSAFE.sub('-', name)}-{UNSAFE.sub('-', version)}"


def document(packages: list[dict[str, str]], version: str) -> dict:
    root = next((p for p in packages if p["name"] == "tafas"), None)
    if root is None:
        raise SystemExit("Cargo.lock does not describe a `tafas` package")

    entries = []
    for package in sorted(packages, key=lambda p: (p["name"], p["version"])):
        entry = {
            "name": package["name"],
            "SPDXID": spdx_id(package["name"], package["version"]),
            "versionInfo": package["version"],
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "externalRefs": [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": f"pkg:cargo/{package['name']}@{package['version']}",
                }
            ],
        }
        if package.get("checksum", "").startswith("sha256:"):
            entry["checksums"] = [
                {
                    "algorithm": "SHA256",
                    "checksumValue": package["checksum"].split(":", 1)[1],
                }
            ]
        entries.append(entry)

    created = time.strftime(
        "%Y-%m-%dT%H:%M:%SZ",
        time.gmtime(int(os.environ.get("SOURCE_DATE_EPOCH", time.time()))),
    )
    return {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"tafas-{version}",
        "documentNamespace": f"https://spdx.org/spdxdocs/tafas-{version}",
        "creationInfo": {
            "created": created,
            "creators": ["Tool: tafas packaging/sbom.py"],
        },
        "documentDescribes": [spdx_id("tafas", version)],
        "packages": entries,
        "relationships": [
            {
                "spdxElementId": "SPDXRef-DOCUMENT",
                "relationshipType": "DESCRIBES",
                "relatedSpdxElement": spdx_id("tafas", version),
            }
        ],
    }


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--version",
        help="the release this document describes; defaults to the crate version in the lock file",
    )
    parser.add_argument("--lock", type=Path, default=ROOT / "Cargo.lock")
    args = parser.parse_args(argv)

    packages = read_packages(args.lock.read_text())
    version = args.version or next(p["version"] for p in packages if p["name"] == "tafas")
    document_text = json.dumps(document(packages, version), indent=2) + "\n"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(document_text)
    print(f"{args.output}: {len(packages)} packages")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
