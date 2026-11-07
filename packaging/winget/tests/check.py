#!/usr/bin/env python3
"""Tests for packaging/winget/generate.py.

    python3 packaging/winget/tests/check.py

Everything runs in a temporary directory: the fixture ZIPs are built here, so the
checks do not need a release, a network or a Windows machine. They do need to know
that the manifests name the exact archive bytes, because that is the only thing
that makes a local `winget install --manifest` the same program as the release.
"""

from __future__ import annotations

import hashlib
import sys
import tempfile
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import generate  # noqa: E402

FAILURES: list[str] = []
PASSED = 0


def check(condition: bool, message: str) -> None:
    global PASSED
    if not condition:
        raise AssertionError(message)
    PASSED += 1


def expect_failure(action, needle: str, message: str) -> None:
    try:
        action()
    except Exception as error:  # noqa: BLE001 - the point is which refusal happened
        check(needle.lower() in str(error).lower(), f"{message} (refused with {error!r})")
        return
    raise AssertionError(f"{message} (nothing was raised)")


def build_archive(path: Path, entries: dict[str, bytes]) -> Path:
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, payload in entries.items():
            archive.writestr(name, payload)
    return path


def run_case(name: str, body) -> None:
    print(f"case: {name}")
    try:
        body()
    except AssertionError as error:
        FAILURES.append(f"{name}: {error}")
        print(f"  FAILED: {error}")
    else:
        print("  ok")


def main() -> int:
    with tempfile.TemporaryDirectory() as workspace:
        root = Path(workspace)
        good = build_archive(
            root / generate.ASSET_NAME,
            {"tafas.exe": b"MZ fake but the right name\n", "LICENSE": b"MIT\n"},
        )
        good_sha = hashlib.sha256(good.read_bytes()).hexdigest().upper()

        def renders_all_three_manifests() -> None:
            output = root / "out"
            manifest_dir, bundle = generate.render("v0.1.0", "0.1.0", good_sha, output)
            check(manifest_dir == output / "0.1.0", "manifests land in a directory named after the version")
            names = sorted(p.name for p in manifest_dir.iterdir())
            check(
                names
                == [
                    "RUdolf1517.tafas.installer.yaml",
                    "RUdolf1517.tafas.locale.en-US.yaml",
                    "RUdolf1517.tafas.yaml",
                ],
                f"exactly the three manifest files were written: {names}",
            )
            installer = (manifest_dir / "RUdolf1517.tafas.installer.yaml").read_text()
            check(
                f"InstallerSha256: {good_sha}" in installer,
                "the installer manifest carries the archive hash, uppercase",
            )
            check(
                f"https://github.com/{generate.REPOSITORY}/releases/download/v0.1.0/"
                f"{generate.ASSET_NAME}" in installer,
                "the installer URL is the release URL for the tag",
            )
            check("InstallerType: zip" in installer, "the installer type is zip")
            check("NestedInstallerType: portable" in installer, "the payload is installed as portable")
            check("RelativeFilePath: tafas.exe" in installer, "the nested file is tafas.exe")
            check("PortableCommandAlias: tafas" in installer, "the alias is tafas")
            check("{{" not in installer, "no template placeholder survived")
            check(bundle.name == "tafas-winget-local-manifests-v0.1.0.zip", f"bundle name: {bundle.name}")
            with zipfile.ZipFile(bundle) as archive:
                check(
                    sorted(archive.namelist())
                    == [f"0.1.0/{name}" for name in sorted(generate.TEMPLATE_NAMES.values())],
                    "the bundle holds the manifests under the version directory",
                )

        def cli_writes_where_it_is_told() -> None:
            output = root / "cli-out"
            code = 0
            try:
                sys.argv = [
                    "generate.py",
                    "--tag",
                    "0.1.0",
                    "--windows-archive",
                    str(good),
                    "--output-dir",
                    str(output),
                ]
                generate.main()
            except SystemExit as exit_error:  # pragma: no cover - argparse only
                code = int(exit_error.code or 0)
            check(code == 0, "the generator exits zero")
            check((output / "0.1.0").is_dir(), "the tag without a leading v is accepted")

        def tag_forms() -> None:
            check(generate.parse_tag("v0.1.0") == ("v0.1.0", "0.1.0"), "a tag is normalised")
            check(generate.parse_tag("0.1.0") == ("v0.1.0", "0.1.0"), "a bare version is normalised")
            expect_failure(lambda: generate.parse_tag("v0.1"), "expected a stable release tag", "a short version is refused")
            expect_failure(
                lambda: generate.parse_tag("v0.1.0-rc1"),
                "expected a stable release tag",
                "a pre-release is refused",
            )

        def archive_shape_is_enforced() -> None:
            extra = build_archive(
                root / "extra.zip",
                {"tafas.exe": b"x", "LICENSE": b"y", "tafas.pdb": b"z"},
            )
            expect_failure(
                lambda: generate.hash_windows_asset(extra),
                "unexpected Windows release ZIP entries",
                "an archive with a third entry is refused",
            )
            missing = build_archive(root / "missing.zip", {"tafas.exe": b"x"})
            expect_failure(
                lambda: generate.hash_windows_asset(missing),
                "unexpected Windows release ZIP entries",
                "an archive without LICENSE is refused",
            )
            empty = build_archive(root / "empty.zip", {})
            expect_failure(
                lambda: generate.hash_windows_asset(empty),
                "unexpected Windows release ZIP entries",
                "an empty archive is refused",
            )
            expect_failure(
                lambda: generate.hash_windows_asset(root / "absent.zip"),
                "is missing",
                "an absent archive is refused",
            )

        def hash_matches_the_bytes_on_disk() -> None:
            check(generate.hash_windows_asset(good) == good_sha, "the hash is of the archive, not of a rebuilt one")
            check(len(good_sha) == 64, "the hash is a SHA-256 digest")

        run_case("three manifests render from a release archive", renders_all_three_manifests)
        run_case("the command line writes where it is told", cli_writes_where_it_is_told)
        run_case("tags normalise and malformed versions are refused", tag_forms)
        run_case("the archive must carry exactly tafas.exe and LICENSE", archive_shape_is_enforced)
        run_case("the hash is the archive's own SHA-256", hash_matches_the_bytes_on_disk)

    print(f"passed: {PASSED}")
    print(f"failed: {len(FAILURES)}")
    for failure in FAILURES:
        print(f"  {failure}")
    return 1 if FAILURES else 0


if __name__ == "__main__":
    raise SystemExit(main())
