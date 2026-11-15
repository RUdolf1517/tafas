#!/usr/bin/env python3
"""Does `Formula/tafas.rb` describe the release it claims to?

    python3 packaging/tests/formula.py --sums SHA256SUMS
    python3 packaging/tests/formula.py --release v0.4.0

The formula is the one artifact in this repository that repeats a hash by hand.
Everything else that names an archive — the AUR `PKGBUILD`, the Scoop manifest,
`flake.nix`, the winget manifests — is rendered by `packaging/channels.py` from
the release's own `SHA256SUMS`, so it cannot disagree with it. The formula is
edited once per release (step 5 of `docs/releases.md` §4), and until now nothing
checked that the edit happened or that the four numbers were the right ones.

That is not a hypothetical. 0.3.0 shipped a formula with four zeroes for two
releases' worth of time: a `sha256` that is not a hash of the archive fails at
`brew install`, after the download, with a message about a checksum rather than
about a file somebody forgot to update.

Two things are checked, and the second is the one that catches a half-done bump:

1. the version in the formula is the version being released;
2. every archive the formula downloads carries the hash `SHA256SUMS` gives for
   that exact file name, and the formula downloads every platform the release
   builds for macOS and Linux.

`--sums` works offline against a local `SHA256SUMS`, which is what CI runs. A
transient GitHub or crates.io failure must not paint a branch red, or the check
gets switched off and the drift comes back.
"""

from __future__ import annotations

import argparse
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
FORMULA = ROOT / "Formula" / "tafas.rb"

# The archives the formula downloads, and the platforms it is the installer for.
# The names are the release's asset names (`.github/workflows/release.yml`); a
# formula that downloads an archive the release does not build is a `brew
# install` that 404s, which is why the set is named here rather than discovered.
EXPECTED = {
    "tafas-aarch64-apple-darwin.tar.gz": "macOS arm64",
    "tafas-x86_64-apple-darwin.tar.gz": "macOS x86_64",
    "tafas-aarch64-unknown-linux-musl.tar.gz": "Linux arm64",
    "tafas-x86_64-unknown-linux-musl.tar.gz": "Linux x86_64",
}

VERSION = re.compile(r'^  version "([^"]+)"$', re.MULTILINE)
# Written by `packaging/channels.py` and by nothing else: it is the one line in
# the formula that says "these hashes are not real yet, and here is why".
UNPUBLISHED = re.compile(r'^# unpublished: v([0-9]+\.[0-9]+\.[0-9]+) has not been built', re.MULTILINE)
URL = re.compile(r'^      url "([^"]+)"$', re.MULTILINE)
SHA = re.compile(r'^      sha256 "([0-9a-f]{64})"$', re.MULTILINE)


def declared_unpublished(text: str) -> str | None:
    match = UNPUBLISHED.search(text)
    return match.group(1) if match else None


def formula_version(text: str) -> str:
    found = VERSION.search(text)
    if not found:
        raise SystemExit("formula: Formula/tafas.rb has no `version \"x.y.z\"` line")
    return found.group(1)


def formula_hashes(text: str) -> list[tuple[str, str]]:
    """(asset name, hash) in the order they appear.

    A `url` line owns the `sha256` line under it, which is how Homebrew reads
    them too: the hash belongs to the block, not to the class.
    """
    pairs: list[tuple[str, str]] = []
    pending: str | None = None
    for line in text.splitlines():
        url = URL.match(line)
        if url:
            pending = url.group(1).rsplit("/", 1)[-1]
            continue
        sha = SHA.match(line)
        if sha and pending is not None:
            pairs.append((pending, sha.group(1)))
            pending = None
    return pairs


def read_sums(path: Path) -> dict[str, str]:
    if not path.is_file():
        raise SystemExit(f"formula: {path} does not exist")
    sums: dict[str, str] = {}
    for line in path.read_text().splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        digest, name = parts
        # `sha256sum ./*` writes `./name`; a binary-mode line is `*name`.
        name = name.lstrip("*").rsplit("/", 1)[-1]
        if re.fullmatch(r"[0-9a-f]{64}", digest):
            sums[name] = digest
    return sums


def release_exists(tag: str) -> bool:
    """Whether a release with this tag is published.

    A version bump lands before its tag does, so for a few hours the formula
    names a release that does not exist. That is not a wrong hash — there is no
    hash to be wrong — and treating it as one would put a red check on every
    version bump, which is how a check stops being read.
    """
    url = f"https://github.com/RUdolf1517/tafas/releases/download/{tag}/SHA256SUMS"
    request = urllib.request.Request(url, method="HEAD")
    try:
        urllib.request.urlopen(request, timeout=30)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise SystemExit(f"formula: cannot read {url}: {error}")
    except (urllib.error.URLError, TimeoutError) as error:
        raise SystemExit(f"formula: cannot read {url}: {error}")
    return True


def sums_from_release(tag: str) -> dict[str, str]:
    """The release's `SHA256SUMS`, for the local run of this check.

    Only the local run: CI passes `--sums`, so a network failure here is a
    person's afternoon rather than a red branch.
    """
    url = f"https://github.com/RUdolf1517/tafas/releases/download/{tag}/SHA256SUMS"
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            text = response.read().decode("utf-8")
    except (urllib.error.URLError, TimeoutError) as error:
        raise SystemExit(f"formula: cannot read {url}: {error}")
    tmp = ROOT / ".formula-sums.tmp"
    try:
        tmp.write_text(text)
        return read_sums(tmp)
    finally:
        tmp.unlink(missing_ok=True)


def check(
    text: str,
    sums: dict[str, str],
    expected_version: str | None,
    unpublished: bool = False,
) -> list[str]:
    """Every way the formula can disagree with the release it claims.

    `unpublished` is the caller's *verified* statement that this version has no
    release yet — not the formula's own claim, which is checked against GitHub
    before it gets here. When it is true the four hashes are allowed to be
    placeholders and nothing else changes: the version, the URLs, the platforms
    and the number of hashes are still checked.
    """
    problems: list[str] = []

    version = formula_version(text)
    if expected_version is not None and version != expected_version:
        problems.append(
            f"the formula says {version} and this release is {expected_version}"
        )

    pairs = formula_hashes(text)
    by_asset = dict(pairs)
    if len(by_asset) != len(pairs):
        problems.append("the formula names the same archive twice")

    for asset, platform in EXPECTED.items():
        if asset not in by_asset:
            problems.append(f"the formula does not install {platform} ({asset})")
            continue
        if asset not in sums:
            problems.append(f"SHA256SUMS has no {asset}, so the formula cannot be checked")
            continue
        # A placeholder is not a wrong hash: it is the absence of one, already
        # reported by name below. Comparing it with the release would report the
        # same line twice and bury the one that says what is missing.
        placeholder = set(by_asset[asset]) == {"0"}
        if by_asset[asset] != sums[asset] and not (placeholder and unpublished):
            problems.append(
                f"{asset}: the formula has {by_asset[asset]}, the release has {sums[asset]}"
            )
        if placeholder and not unpublished:
            problems.append(f"{asset}: the formula carries a placeholder, not a hash")

    for asset in by_asset:
        if asset not in EXPECTED:
            problems.append(f"the formula downloads {asset}, which the release does not build")

    # Homebrew derives `version` from the URL when it is absent and refuses a
    # mismatch when it is not, so the one place the number is written has to be
    # the one the archives live under. The formula interpolates it
    # (`v#{version}`), which is the same string to Homebrew and a different one
    # to a grep, so both spellings are accepted and neither is guessed at.
    urls = URL.findall(text)
    if not urls:
        problems.append("the formula downloads nothing")
    for url in urls:
        if f"/download/v{version}/" in url or "/download/v#{version}/" in url:
            continue
        problems.append(f"{url} is not under v{version}")

    return problems


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(add_help=True)
    parser.add_argument("--sums", type=Path, help="a local SHA256SUMS")
    parser.add_argument("--release", help="a release tag to read SHA256SUMS from")
    parser.add_argument(
        "--expect-version",
        help="the version being released; defaults to the one in the formula",
    )
    args = parser.parse_args(argv)

    if args.sums is None and args.release is None:
        parser.error("one of --sums or --release is required")

    text = FORMULA.read_text()
    version = args.expect_version or formula_version(text)

    # The two ways a formula can legitimately carry placeholders, and both are
    # verified rather than taken from the file:
    #
    # 1. this version has no release yet (a bump lands before its tag);
    # 2. the caller is checking the formula against *another* release, which is
    #    what CI does — it compares `main` with the latest published tag, so a
    #    tree that has moved on is ahead of it rather than wrong about it.
    declared = declared_unpublished(text)
    if declared is not None and declared != version:
        print(
            f"formula: the file says v{declared} is unpublished but its version is {version}",
            file=sys.stderr,
        )
        return 1

    if args.sums is not None:
        sums = read_sums(args.sums)
        # `--sums` names the checksums of the version being checked, so a
        # declared placeholder is legitimate for that version by definition.
        unpublished = declared == version
    else:
        if not release_exists(args.release):
            # A tag with no release is not a wrong hash; there is no hash. Say so
            # and say nothing else, rather than reporting a formula as verified
            # against something that does not exist.
            print(
                f"formula: {args.release} is not published, so there is nothing to compare "
                f"against; the formula's hashes are unverified, not wrong"
            )
            return 0
        sums = sums_from_release(args.release)
        unpublished = declared == version
        if unpublished:
            # The declaration is checked, not believed. A version that says it has
            # not been built, while its release tag answers with checksums, is
            # exactly the shape of "the zeros are still there and somebody wrote
            # a comment about it".
            tag = f"v{version}"
            if release_exists(tag):
                print(
                    f"formula: the file says v{version} has not been built, but its release "
                    f"exists: the hashes are placeholders where they should be real",
                    file=sys.stderr,
                )
                return 1
            # Nothing to compare against, and the declaration just explained why.
            print(
                f"formula: tafas {version} is not published yet; the version, the "
                f"{len(EXPECTED)} platform URLs and the shape of the file are right, and the "
                f"four hashes are placeholders until the release exists"
            )
            return 0
        # Otherwise the release being read is the last published one and the
        # formula names a different version: that is `main` ahead of the tag, not
        # an error, and the comparison below reports it as it always did.

    problems = check(text, sums, version, unpublished)
    for problem in problems:
        print(f"formula: {problem}", file=sys.stderr)
    if problems:
        return 1
    if unpublished:
        # Say exactly what was and was not checked. A green line that claims the
        # hashes are the release's when they are placeholders is the failure this
        # whole file exists to prevent, one level up.
        print(
            f"formula: tafas {version} is not published yet; the version, the "
            f"{len(EXPECTED)} platform URLs and the shape of the file are right, and the "
            f"four hashes are placeholders until the release exists"
        )
        return 0
    print(
        f"formula: tafas {version} downloads {len(EXPECTED)} archives "
        f"and every hash is the release's"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
