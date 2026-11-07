#!/bin/sh
# Builds the .deb and .rpm from the Linux tarballs, so that both describe the
# same binary the archives carry. Requires `nfpm` (go install
# github.com/goreleaser/nfpm/v2/cmd/nfpm@latest).
#
#   packaging/linux-packages.sh <version> <artifacts-dir>
#
# The tarballs are expected in <artifacts-dir>: tafas-x86_64-unknown-linux-musl.tar.gz
# and tafas-aarch64-unknown-linux-musl.tar.gz.
set -eu

version="${1:?usage: linux-packages.sh <version> <artifacts-dir>}"
dir="${2:?usage: linux-packages.sh <version> <artifacts-dir>}"
version="${version#v}"

cd "$dir"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

command -v nfpm >/dev/null 2>&1 || {
  printf '%s\n' "linux-packages: nfpm is not installed; skipping package build" >&2
  exit 0
}

make_package() {
  arch_triple="$1"   # aarch64-unknown-linux-musl
  deb_arch="$2"      # arm64
  rpm_arch="$3"      # aarch64
  tarball="tafas-${arch_triple}.tar.gz"
  [ -f "$tarball" ] || { printf '%s\n' "linux-packages: missing $tarball" >&2; return 1; }

  stage="$work/$deb_arch"
  mkdir -p "$stage"
  tar -C "$stage" -xzf "$tarball"

  cat > "$work/nfpm-$deb_arch.yaml" <<EOF
name: tafas
arch: $deb_arch
platform: linux
version: $version
section: utils
priority: optional
maintainer: RUdolf1517 <rudolfzinovev@protonmail.com>
description: |
  Capture a video from YouTube, TikTok or X as one playable MP4.
  Picture and sound are copied and the container index is rebuilt: nothing is
  re-encoded and no external program is run.
homepage: https://github.com/RUdolf1517/tafas
license: MIT
contents:
  - src: $stage/tafas
    dst: /usr/bin/tafas
  - src: $stage/man/tafas.1
    dst: /usr/share/man/man1/tafas.1
  - src: $stage/completions/tafas.bash
    dst: /usr/share/bash-completion/completions/tafas
  - src: $stage/completions/_tafas
    dst: /usr/share/zsh/vendor-completions/_tafas
  - src: $stage/completions/tafas.fish
    dst: /usr/share/fish/vendor_completions.d/tafas.fish
EOF

  nfpm package --config "$work/nfpm-$deb_arch.yaml" --packager deb --target "tafas_${deb_arch}.deb"
  nfpm package --config "$work/nfpm-$deb_arch.yaml" --packager rpm --target "$work/tafas.${rpm_arch}.rpm"
  # nfpm names RPMs by arch; rename to the asset name the README documents.
  mv "$work/tafas.${rpm_arch}.rpm" "tafas.${rpm_arch}.rpm" 2>/dev/null || \
    find "$work" -maxdepth 1 -name 'tafas*.rpm' -exec mv {} "tafas.${rpm_arch}.rpm" \;
}

make_package x86_64-unknown-linux-musl amd64 x86_64
make_package aarch64-unknown-linux-musl arm64 aarch64

ls -l ./*.deb ./*.rpm
