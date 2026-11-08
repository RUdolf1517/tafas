#!/bin/sh
# Builds the Alpine package from the static Linux tarball.
#
#   packaging/apk.sh <version> <artifacts-dir>
#
# The tarballs are expected in <artifacts-dir>: tafas-x86_64-unknown-linux-musl.tar.gz
# and tafas-aarch64-unknown-linux-musl.tar.gz — the same two the .deb and .rpm are
# built from, and the same ones the AUR package names.
#
# Two ways to produce an .apk, and this script uses the second:
#
# 1. `abuild` inside an Alpine container, with an APKBUILD and a source tarball.
#    Faithful, but it needs the whole abuild toolchain and an Alpine user for
#    every architecture.
# 2. `apk mkpkg`, from `apk-tools` 3, which builds a package from a staging tree
#    and a control file — no Alpine container, no APKBUILD. The result is a
#    normal `.apk`: a gzipped tar with `.PKGINFO` and an optional `.SIGN`.
#
# This script is (2). An unsigned `.apk` is what a reader gets, and `apk` accepts
# one when it is installed by path. That is stated rather than hidden: signing
# needs the Alpine key this repository does not hold, exactly like minisign.
set -eu

version="${1:?usage: apk.sh <version> <artifacts-dir>}"
dir="${2:?usage: apk.sh <version> <artifacts-dir>}"
version="${version#v}"

cd "$dir"

if ! apk mkpkg --help >/dev/null 2>&1; then
  printf '%s\n' "apk: 'apk mkpkg' is not available (it needs apk-tools 3); skipping the .apk" >&2
  exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

# Alpine calls x86_64 "x86_64" and aarch64 "aarch64", unlike Debian's amd64/arm64.
make_apk() {
  apk_arch="$1"
  triple="$2"
  tarball="tafas-${triple}.tar.gz"
  [ -f "$tarball" ] || { printf '%s\n' "apk: missing $tarball" >&2; return 1; }

  stage="$work/stage-$apk_arch"
  mkdir -p "$stage/usr/bin" "$stage/usr/share/man/man1" \
    "$stage/usr/share/bash-completion/completions" \
    "$stage/usr/share/zsh/site-functions" \
    "$stage/usr/share/fish/vendor_completions.d" \
    "$stage/lib/apk/db"
  tar -C "$stage" -xzf "$tarball"
  # The tarball has `tafas` at its root; the manual page and completions in
  # `man/` and `completions/`, which the archive already placed correctly.

  cat > "$stage/.PKGINFO" <<EOF
pkgname = tafas
pkgver = ${version}-r0
pkgdesc = Capture a video from YouTube, TikTok, X, Vimeo, Reddit, SoundCloud or Twitch into one file
url = https://github.com/RUdolf1517/tafas
builddate = $(date -u +%s)
packager = RUdolf1517 <rudolfzinovev@protonmail.com>
size = $(du -sk "$stage/usr" | cut -f1)
arch = $apk_arch
license = MIT
origin = tafas
# Static musl: not even libc is a dependency, which is the point of the tarball.
depend =
EOF

  apk mkpkg --root "$stage" --info "$stage/.PKGINFO" --output "tafas-${version}-${apk_arch}.apk"
}

make_apk x86_64 x86_64-unknown-linux-musl
make_apk aarch64 aarch64-unknown-linux-musl

ls -l ./*.apk
