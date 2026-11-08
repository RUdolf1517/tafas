#!/usr/bin/env python3
"""Checks the `.apk` that `packaging/apk.py` writes, without an Alpine machine.

    packaging/tests/apk.py

What is checked is what made the first attempt at this package fail to exist and
what would make the next one fail to install:

* the file is two gzip members, control then data, and nothing else — the shape
  `apk-v2(5)` describes and every apk-tools since 2.0 reads;
* `.PKGINFO` carries the fields apk requires, the version it was asked for, and
  a `datahash` that is the SHA-256 of the *second* member's compressed bytes;
* the data member holds every file the `.deb`/`.rpm` hold, at the paths Alpine
  expects, each preceded by its own checksum pax header — the pax record length
  is counted in bytes and the file it precedes must keep its own name;
* two runs of the same input produce the same bytes, which is what makes the
  checksum in `SHA256SUMS` mean anything.
"""
from __future__ import annotations

import gzip
import hashlib
import io
import os
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PASSED = 0
FAILED: list[str] = []


def check(condition: bool, what: str) -> None:
    global PASSED
    if condition:
        PASSED += 1
    else:
        FAILED.append(what)
        print(f"not ok: {what}")


def gunzip_members(raw: bytes) -> list[bytes]:
    """Every gzip member in `raw`, in order."""
    out: list[bytes] = []
    rest = raw
    while rest:
        d = __import__("zlib").decompressobj(31)
        out.append(d.decompress(rest))
        consumed = len(rest) - len(d.unused_data)
        if consumed == 0:
            raise AssertionError("gzip member consumed nothing")
        rest = d.unused_data
    return out


def pax_value(member: bytes, prefix: bytes) -> list[bytes]:
    """The values of `prefix` records in a data member, in order."""
    values: list[bytes] = []
    i = 0
    while i + 512 <= len(member):
        header = member[i : i + 512]
        if header[:1] == b"\0":
            break
        size = int(header[124:136].strip(b"\0 ") or b"0", 8)
        if chr(header[156]) == "x":
            payload = member[i + 512 : i + 512 + size]
            while payload:
                # `<decimal length><space><key>=<value>\n`, the length counting
                # itself — so the record is a prefix of the payload, not a line.
                length = int(payload.split(b" ", 1)[0])
                record = payload[:length].split(b" ", 1)[1]
                key, _, value = record.partition(b"=")
                if key.strip().startswith(prefix):
                    values.append(value.rstrip(b"\n"))
                payload = payload[length:]
        i += 512 + ((size + 511) // 512) * 512
    return values


def build(artifacts: Path, version: str) -> Path:
    env = dict(os.environ, SOURCE_DATE_EPOCH="1767225600")
    subprocess.run(
        [sys.executable, str(ROOT / "packaging/apk.py"), version, str(artifacts)],
        check=True,
        env=env,
        capture_output=True,
    )
    return artifacts / f"tafas-{version}-x86_64.apk"


def self_test() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        artifacts = Path(tmp)
        # A stand-in tarball with the same five entries the release archive has.
        stage = artifacts / "stage"
        (stage / "man").mkdir(parents=True)
        (stage / "completions").mkdir()
        (stage / "tafas").write_bytes(b"#!/bin/sh\necho tafas\n")
        (stage / "man/tafas.1").write_bytes(b".TH TAFAS 1\n")
        (stage / "completions/tafas.bash").write_bytes(b"complete -F _t tafas\n")
        (stage / "completions/_tafas").write_bytes(b"#compdef tafas\n")
        (stage / "completions/tafas.fish").write_bytes(b"complete -c tafas\n")
        for triple in ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"):
            # The entries are named without a `./` prefix, exactly as the
            # release job's `tar czf ... tafas LICENSE man completions` writes
            # them; the packer looks them up by name.
            subprocess.run(
                [
                    "tar",
                    "czf",
                    str(artifacts / f"tafas-{triple}.tar.gz"),
                    "tafas",
                    "man",
                    "completions",
                ],
                check=True,
                cwd=stage,
                capture_output=True,
            )

        apk_path = build(artifacts, "0.2.0")
        check(apk_path.is_file(), "the packer writes tafas-0.2.0-x86_64.apk")
        raw = apk_path.read_bytes()
        check(raw[:2] == b"\x1f\x8b", "the .apk starts with a gzip member")

        members = gunzip_members(raw)
        check(len(members) == 2, f"two gzip members: signatures are absent, control and data are not ({len(members)})")

        control, data = members
        with tarfile.open(fileobj=io.BytesIO(control)) as tar:
            names = tar.getnames()
            check(names == [".PKGINFO"], f"control holds exactly .PKGINFO, not {names}")
            pkginfo = tar.extractfile(".PKGINFO").read().decode()

        def field(key: str) -> str:
            for line in pkginfo.splitlines():
                if line.startswith(f"{key} = "):
                    return line.split(" = ", 1)[1]
            return ""

        check(field("pkgname") == "tafas", "pkgname")
        check(field("pkgver") == "0.2.0-r0", f"pkgver is 0.2.0-r0, not {field('pkgver')!r}")
        check(field("arch") == "x86_64", f"arch is x86_64, not {field('arch')!r}")
        check(field("license") == "MIT", "license")
        check(field("origin") == "tafas", "origin")
        check(field("depend") == "", "no dependencies: the binary is static")
        check(
            field("size").isdigit() and int(field("size")) > 0,
            "the installed size is in bytes, as abuild and melange write it",
        )
        # abuild and melange both hash the compressed data stream, and this is
        # the field that ties the two members together.
        second = raw[raw.index(b"\x1f\x8b\x08", 2) :] if len(members) == 2 else b""
        check(
            field("datahash") == hashlib.sha256(second).hexdigest(),
            "datahash is the sha256 of the data member's compressed bytes",
        )

        payload = io.BytesIO(data)
        with tarfile.open(fileobj=payload) as tar:
            files = {m.name: m for m in tar.getmembers() if m.isreg()}
        for path in (
            "usr/bin/tafas",
            "usr/share/man/man1/tafas.1",
            "usr/share/bash-completion/completions/tafas",
            "usr/share/zsh/site-functions/_tafas",
            "usr/share/fish/vendor_completions.d/tafas.fish",
        ):
            check(path in files, f"the data member carries {path}")
        check(
            oct(files["usr/bin/tafas"].mode) == "0o755",
            f"the binary is executable, mode {oct(files['usr/bin/tafas'].mode)}",
        )

        digests = pax_value(data, b"APK-TOOLS.checksum.SHA1")
        check(len(digests) == 5, f"one checksum per file, not {len(digests)}")
        check(
            digests and digests[0] == hashlib.sha1(b"#!/bin/sh\necho tafas\n").hexdigest().encode(),
            "the checksum is the file's own sha1",
        )
        check(not pax_value(data, b"path"), "no pax record renames the file it precedes")

        # Reproducible: same inputs, same bytes. Without this the checksum in
        # SHA256SUMS would be a checksum of whatever time it was built.
        again = build(artifacts, "0.2.0")
        check(again.read_bytes() == raw, "two builds of one input are byte-identical")

        # And the second architecture, because the arch field is per-package.
        aarch64 = artifacts / "tafas-0.2.0-aarch64.apk"
        check(aarch64.is_file(), "the packer writes the aarch64 package")
        with tarfile.open(fileobj=io.BytesIO(gunzip_members(aarch64.read_bytes())[0])) as tar:
            check(
                "arch = aarch64" in tar.extractfile(".PKGINFO").read().decode(),
                "the aarch64 package says arch = aarch64, not arm64",
            )


if __name__ == "__main__":
    self_test()
    print(f"passed: {PASSED} failed: {len(FAILED)}")
    for failure in FAILED:
        print(f"  - {failure}")
    sys.exit(1 if FAILED else 0)
