#!/usr/bin/env python3
"""The three channels that are a file each: AUR, Scoop and a flake.

    python3 packaging/channels.py --tag v0.2.0 --output-dir dist/channels

Each of them names a binary that the release already built and a hash of that
binary's archive. So each of them is generated from `SHA256SUMS` rather than
written by hand: a hand-written hash is a hash that is right until the next
release, and the failure it produces is an install that fails after downloading
four megabytes.

What is generated, and from what:

| Channel | Files | Hash it repeats |
| --- | --- | --- |
| AUR | `PKGBUILD`, `.SRCINFO` | the Linux tarball for each architecture |
| Scoop | `tafas.json` | the Windows ZIP |
| flake | `flake.nix` | the Linux and macOS tarballs |

The outputs are published with the release as `tafas-channels-<version>.tar.gz`,
because the AUR package and a `flake.nix` a reader can copy is the point of
shipping them. They are not checked in: a file that repeats the hash of an
archive is stale the moment the next archive is built, and a checked-in copy
would be a second source of truth that goes wrong quietly. `--self-test` renders
the same files from a stand-in release and asserts the properties, which is what
runs on every pull request; `--tag` with `--artifacts` is what runs on a tag.

Nothing here publishes: an AUR package is pushed by whoever holds the AUR key,
and that is a human step, stated in `docs/releases.md` §6.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DESCRIPTION = (
    "Capture a video from YouTube, TikTok, X, Vimeo, Reddit, SoundCloud or Twitch "
    "into one file, with no re-encoding and no external binaries"
)


# The assets a channel consumes. The release builds exactly these names
# (`.github/workflows/release.yml`), and `tests/packaging.rs` fails if the names
# here and the names there drift.
LINUX_X86 = "tafas-x86_64-unknown-linux-musl.tar.gz"
LINUX_ARM = "tafas-aarch64-unknown-linux-musl.tar.gz"
DARWIN_ARM = "tafas-aarch64-apple-darwin.tar.gz"
DARWIN_X86 = "tafas-x86_64-apple-darwin.tar.gz"
WINDOWS = "tafas-x86_64-pc-windows-msvc.zip"

DEPENDS = [
    LINUX_X86,
    LINUX_ARM,
    DARWIN_ARM,
    DARWIN_X86,
    WINDOWS,
]


def read_sums(artifacts: Path) -> dict[str, str]:
    """`SHA256SUMS` as a name → hash mapping, or the assets hashed directly.

    The release renders these files *before* it writes `SHA256SUMS`, because the
    channels tarball is one of the artifacts that file has to cover. So when the
    checksum file is not there yet, the assets are hashed in place — the digest
    is the same either way, and hashing them twice is cheaper than an ordering
    in which one artifact is not in the checksums.
    """
    sums = artifacts / "SHA256SUMS"
    if not sums.is_file():
        present = {name: sha256_file(artifacts / name) for name in DEPENDS if (artifacts / name).is_file()}
        if not present:
            raise SystemExit(
                f"channels: neither {sums} nor any of the {len(DEPENDS)} release assets is in {artifacts}"
            )
        print(
            f"channels: {sums} is not written yet; hashing the {len(present)} assets in place",
            file=sys.stderr,
        )
        return present
    out: dict[str, str] = {}
    for line in sums.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        parts = line.split()
        if len(parts) != 2:
            continue
        digest, name = parts
        # `sha256sum ./*` writes `./name`, and a binary-mode line is `*name`;
        # the release writes the first, so the path has to be reduced to a name
        # before it can be looked up.
        name = name.lstrip("*").rsplit("/", 1)[-1]
        if re.fullmatch(r"[0-9a-f]{64}", digest):
            out[name] = digest
    return out


def need(sums: dict[str, str], name: str) -> str:
    if name not in sums:
        raise SystemExit(f"channels: SHA256SUMS has no {name}; the release did not build it")
    return sums[name]


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def nix_hash(digest: str) -> str:
    """A hex digest as the SRI form Nix wants (`sha256-<base64>`).

    `fetchurl` accepts a hex hash only in the deprecated `sha256 = "…"` form; the
    `hash` attribute is SRI, and a hex string there is rejected with a message
    about the wrong number of characters rather than about the format.
    """
    raw = bytes.fromhex(digest)
    return "sha256-" + base64.b64encode(raw).decode("ascii")


def version_of(tag: str) -> str:
    """`v0.2.0` → `0.2.0`. A tag without the `v` is accepted, because a local
    rendering should not depend on how the tag was typed."""
    version = tag[1:] if tag.startswith("v") else tag
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise SystemExit(f"channels: {tag!r} is not a vX.Y.Z release tag")
    return version


# --- AUR ------------------------------------------------------------------


def aur_pkgbuild(version: str, sums: dict[str, str]) -> str:
    return f"""# Maintainer: RUdolf1517 <rudolfzinovev@protonmail.com>
# Generated by packaging/channels.py from the release's SHA256SUMS. An edit here
# is overwritten by the next release, so a change belongs in the generator.
pkgname=tafas-bin
pkgver={version}
pkgrel=1
pkgdesc="{DESCRIPTION}"
arch=('x86_64' 'aarch64')
url="https://github.com/RUdolf1517/tafas"
license=('MIT')
# The binary is static: not even glibc. That is what makes one tarball work on
# every distribution, and it is why there is nothing to depend on here.
depends=()
provides=('tafas')
conflicts=('tafas')
options=('!strip' '!debug')
source_x86_64=("${{pkgname}}-${{pkgver}}-x86_64.tar.gz::${{url}}/releases/download/v${{pkgver}}/{LINUX_X86}")
source_aarch64=("${{pkgname}}-${{pkgver}}-aarch64.tar.gz::${{url}}/releases/download/v${{pkgver}}/{LINUX_ARM}")
sha256sums_x86_64=('{need(sums, LINUX_X86)}')
sha256sums_aarch64=('{need(sums, LINUX_ARM)}')

package() {{
  install -Dm755 "${{srcdir}}/tafas" "${{pkgdir}}/usr/bin/tafas"
  install -Dm644 "${{srcdir}}/man/tafas.1" "${{pkgdir}}/usr/share/man/man1/tafas.1"
  install -Dm644 "${{srcdir}}/completions/tafas.bash" \\
    "${{pkgdir}}/usr/share/bash-completion/completions/tafas"
  install -Dm644 "${{srcdir}}/completions/_tafas" \\
    "${{pkgdir}}/usr/share/zsh/site-functions/_tafas"
  install -Dm644 "${{srcdir}}/completions/tafas.fish" \\
    "${{pkgdir}}/usr/share/fish/vendor_completions.d/tafas.fish"
  install -Dm644 "${{srcdir}}/LICENSE" "${{pkgdir}}/usr/share/licenses/${{pkgname}}/LICENSE"
}}
"""


def aur_srcinfo(version: str, sums: dict[str, str]) -> str:
    """The `.SRCINFO` AUR reads, rendered from the same values as the PKGBUILD.

    AUR refuses a `PKGBUILD` whose `.SRCINFO` disagrees with it, so the two are
    rendered together and both are checked in.
    """
    return f"""pkgbase = tafas-bin
\tpkgdesc = {DESCRIPTION}
\tpkgver = {version}
\tpkgrel = 1
\turl = https://github.com/RUdolf1517/tafas
\tarch = x86_64
\tarch = aarch64
\tlicense = MIT
\tprovides = tafas
\tconflicts = tafas
\toptions = !strip
\toptions = !debug
\tsource_x86_64 = tafas-bin-{version}-x86_64.tar.gz::https://github.com/RUdolf1517/tafas/releases/download/v{version}/{LINUX_X86}
\tsource_aarch64 = tafas-bin-{version}-aarch64.tar.gz::https://github.com/RUdolf1517/tafas/releases/download/v{version}/{LINUX_ARM}
\tsha256sums_x86_64 = {need(sums, LINUX_X86)}
\tsha256sums_aarch64 = {need(sums, LINUX_ARM)}

pkgname = tafas-bin
"""


# --- Scoop ----------------------------------------------------------------


def scoop_manifest(version: str, sums: dict[str, str]) -> str:
    return (
        "{\n"
        '  "$schema": "https://raw.githubusercontent.com/ScoopInstaller/Scoop/master/schema.json",\n'
        '  "version": "' + version + '",\n'
        '  "description": "' + DESCRIPTION + '",\n'
        '  "homepage": "https://github.com/RUdolf1517/tafas",\n'
        '  "license": "MIT",\n'
        '  "architecture": {\n'
        '    "64bit": {\n'
        f'      "url": "https://github.com/RUdolf1517/tafas/releases/download/v{version}/{WINDOWS}",\n'
        f'      "hash": "{need(sums, WINDOWS)}"\n'
        "    }\n"
        "  },\n"
        '  "bin": "tafas.exe",\n'
        '  "checkver": { "github": "https://github.com/RUdolf1517/tafas" },\n'
        '  "autoupdate": {\n'
        '    "architecture": {\n'
        '      "64bit": {\n'
        '        "url": "https://github.com/RUdolf1517/tafas/releases/download/v$version/'
        + WINDOWS
        + '"\n'
        "      }\n"
        "    }\n"
        "  }\n"
        "}\n"
    )


# --- flake ----------------------------------------------------------------


def flake_nix(version: str, sums: dict[str, str]) -> str:
    return f"""# Generated by packaging/channels.py from the release's SHA256SUMS.
#
# A flake that builds tafas from source and installs the prebuilt binary are two
# different promises, and this one is the second: `nix profile install` on it
# fetches the same static tarball the release publishes, so what a Nix user runs
# is what everyone else runs. The `cargoLock` variant would need a full Rust
# build per platform and a vendor hash per platform; that is a different flake,
# not a better one.
{{
  description = "tafas {version} — no re-encoding, no external binaries";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = {{ nixpkgs, ... }}:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      sources = {{
        x86_64-linux = {{
          url = "https://github.com/RUdolf1517/tafas/releases/download/v{version}/{LINUX_X86}";
          hash = "{nix_hash(need(sums, LINUX_X86))}";
        }};
        aarch64-linux = {{
          url = "https://github.com/RUdolf1517/tafas/releases/download/v{version}/{LINUX_ARM}";
          hash = "{nix_hash(need(sums, LINUX_ARM))}";
        }};
        aarch64-darwin = {{
          url = "https://github.com/RUdolf1517/tafas/releases/download/v{version}/{DARWIN_ARM}";
          hash = "{nix_hash(need(sums, DARWIN_ARM))}";
        }};
        x86_64-darwin = {{
          url = "https://github.com/RUdolf1517/tafas/releases/download/v{version}/{DARWIN_X86}";
          hash = "{nix_hash(need(sums, DARWIN_X86))}";
        }};
      }};
      each = f: nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${{system}});
    in
    {{
      packages = each (system: pkgs: {{
        default = pkgs.stdenv.mkDerivation {{
          pname = "tafas";
          version = "{version}";
          src = pkgs.fetchurl sources.${{system}};
          # The tarball holds a static binary, a manual page and completions —
          # nothing to configure, nothing to link, nothing to strip.
          dontBuild = true;
          dontStrip = true;
          installPhase = ''
            mkdir -p $out/bin $out/share/man/man1
            install -m755 tafas $out/bin/tafas
            install -m644 man/tafas.1 $out/share/man/man1/tafas.1
            mkdir -p $out/share/bash-completion/completions \\
              $out/share/zsh/site-functions $out/share/fish/vendor_completions.d
            install -m644 completions/tafas.bash $out/share/bash-completion/completions/tafas
            install -m644 completions/_tafas $out/share/zsh/site-functions/_tafas
            install -m644 completions/tafas.fish $out/share/fish/vendor_completions.d/tafas.fish
          '';
          meta = with pkgs.lib; {{
            description = "{DESCRIPTION}";
            homepage = "https://github.com/RUdolf1517/tafas";
            license = licenses.mit;
            platforms = systems;
            mainProgram = "tafas";
          }};
        }};
      }});
      formatter = each (system: pkgs: pkgs.nixfmt-rfc-style);
    }};
}}
"""


def render(version: str, sums: dict[str, str]) -> dict[str, str]:
    """Every generated file, by path relative to the repository root."""
    return {
        "packaging/aur/PKGBUILD": aur_pkgbuild(version, sums),
        "packaging/aur/.SRCINFO": aur_srcinfo(version, sums),
        "packaging/scoop/tafas.json": scoop_manifest(version, sums),
        "packaging/flake/flake.nix": flake_nix(version, sums),
    }


def self_test() -> int:
    """Renders from a synthetic release and checks what a user would notice.

    Nothing is checked in and nothing is compared with the repository: a
    generated file that repeats a hash of a release is stale the moment the next
    release is built, and a checked-in copy would be a second source of truth
    that goes wrong quietly. So the file is generated at release time from that
    release's `SHA256SUMS`, and what runs on every pull request is this: the
    generator, over a stand-in release, with the properties asserted.
    """
    import json
    import tempfile

    problems: list[str] = []
    version = "9.9.9"
    with tempfile.TemporaryDirectory() as work:
        artifacts = Path(work) / "artifacts"
        artifacts.mkdir()
        sums: dict[str, str] = {}
        # Different bytes per asset, so a hash copied from the wrong line is a
        # test failure rather than a coincidence.
        for index, name in enumerate(DEPENDS):
            asset = artifacts / name
            asset.write_bytes(f"{name} {index}\n".encode())
            sums[name] = sha256_file(asset)
        (artifacts / "SHA256SUMS").write_text(
            "".join(f"{digest}  ./{name}\n" for name, digest in sums.items())
        )

        files = render(version, read_sums(artifacts))
        for path, text in files.items():
            if not text.endswith("\n"):
                problems.append(f"{path} does not end with a newline")
            for name in DEPENDS:
                if name in text and sums[name] not in text and nix_hash(sums[name]) not in text:
                    problems.append(f"{path} names {name} without its hash")

        pkgbuild = files["packaging/aur/PKGBUILD"]
        for needle in ("pkgver=9.9.9", "sha256sums_x86_64", "sha256sums_aarch64",
                       "arch=('x86_64' 'aarch64')", "install -Dm755"):
            if needle not in pkgbuild:
                problems.append(f"PKGBUILD has no {needle!r}")
        if "pkgver=v9.9.9" in pkgbuild:
            problems.append("PKGBUILD repeats the tag's v in pkgver")

        srcinfo = files["packaging/aur/.SRCINFO"]
        for needle in ("pkgbase = tafas-bin", f"pkgver = {version}", "sha256sums_aarch64"):
            if needle not in srcinfo:
                problems.append(f".SRCINFO has no {needle!r}")

        try:
            manifest = json.loads(files["packaging/scoop/tafas.json"])
            if manifest["version"] != version:
                problems.append("the Scoop manifest names the wrong version")
            if manifest["architecture"]["64bit"]["hash"] != sums[WINDOWS]:
                problems.append("the Scoop manifest hashes something other than the ZIP")
        except (json.JSONDecodeError, KeyError) as error:
            problems.append(f"the Scoop manifest is not the JSON Scoop reads: {error}")

        flake = files["packaging/flake/flake.nix"]
        for name in (LINUX_X86, LINUX_ARM, DARWIN_ARM, DARWIN_X86):
            if nix_hash(sums[name]) not in flake:
                problems.append(f"flake.nix does not carry {name} as an SRI hash")
        if flake.count("{") != flake.count("}"):
            problems.append("flake.nix has unbalanced braces")

        # Twice, so a change in dict ordering or a timestamp cannot slip in and
        # make a release's files differ from a rerun of the same tag.
        if render(version, sums) != files:
            problems.append("rendering twice produced different bytes")

    for problem in problems:
        print(f"channels: {problem}", file=sys.stderr)
    if not problems:
        print("channels: the generator renders the AUR, Scoop and flake files")
    return 1 if problems else 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", help="the release tag, e.g. v0.2.0")
    parser.add_argument(
        "--artifacts",
        help="directory holding SHA256SUMS and the assets it names; the assets are hashed "
        "and compared with the file, so a SHA256SUMS that disagrees with the bytes beside "
        "it is caught here rather than by a user",
    )
    parser.add_argument("--output-dir", help="write the tree below this directory")
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="render from a synthetic release and check the result; this is what CI runs",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    if not args.tag:
        raise SystemExit("channels: --tag is required (or --self-test)")
    if not args.artifacts:
        raise SystemExit("channels: --artifacts is required (or --self-test)")

    version = version_of(args.tag)
    artifacts = Path(args.artifacts)
    sums = read_sums(artifacts)
    for name in DEPENDS:
        need(sums, name)
        asset = artifacts / name
        if asset.is_file() and sha256_file(asset) != sums[name]:
            raise SystemExit(
                f"channels: {name} does not hash to the digest SHA256SUMS records"
            )

    files = render(version, sums)
    out = Path(args.output_dir) if args.output_dir else None
    for path, text in files.items():
        target = (out / path.removeprefix("packaging/")) if out else Path(path)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
        print(f"channels: wrote {target}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
