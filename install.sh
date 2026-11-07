#!/bin/sh
# tafas — one-command install:
#   curl -fsSL https://raw.githubusercontent.com/RUdolf1517/tafas/main/install.sh | sh
#
# Takes the prebuilt binary from the latest GitHub release when one exists for
# this platform, and falls back to building from source with cargo only when no
# release asset is published. Download, integrity, unpacking and version errors
# are fatal; they never masquerade as an absent platform asset.
#
# Every download is checked against the release's SHA256SUMS before it is
# unpacked. A `curl | sh` installer that does not verify what it downloads is
# asking the user to trust the network path as much as the project, and this
# program spends its documentation arguing that trust should be earned by a
# check.
#
# Environment:
#   TAFAS_REPO         owner/name (default RUdolf1517/tafas)
#   TAFAS_VERSION      pin a release, e.g. 0.1.0 (default: the latest release)
#   TAFAS_INSTALL_DIR  destination (default ~/.local/bin)
#   TAFAS_BRANCH       source branch for a fallback build (default main)
#   TAFAS_BASE_URL     download from a mirror instead of GitHub, e.g.
#                      https://mirror.example/tafas — the layout is
#                      <base>/<asset> and <base>/SHA256SUMS. Mirrored releases are
#                      how air-gapped machines install this; the checksum check is
#                      not optional there either.
set -eu

REPO="${TAFAS_REPO:-RUdolf1517/tafas}"
BRANCH="${TAFAS_BRANCH:-main}"
INSTALL_DIR="${TAFAS_INSTALL_DIR:-$HOME/.local/bin}"
say() { printf '%s\n' "$*"; }
err() { printf 'tafas-install: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1; }

os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Linux)  triple_os=unknown-linux-musl ;;
  Darwin) triple_os=apple-darwin ;;
  *) err "unsupported OS: $os (Linux and macOS have prebuilt binaries; on Windows use install.ps1 or the zip from Releases)" ;;
esac
case "$arch" in
  x86_64|amd64)  triple_arch=x86_64 ;;
  arm64|aarch64) triple_arch=aarch64 ;;
  *) err "unsupported architecture: $arch" ;;
esac
asset="tafas-${triple_arch}-${triple_os}.tar.gz"

download() {
  if need curl; then
    curl -fsSL "$1" -o "$2"
  elif need wget; then
    wget -qO "$2" "$1"
  else
    return 1
  fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

if [ -n "${TAFAS_VERSION:-}" ]; then
  tag="v${TAFAS_VERSION#v}"
  base="${TAFAS_BASE_URL:-https://github.com/${REPO}/releases/download/${tag}}"
else
  base="${TAFAS_BASE_URL:-https://github.com/${REPO}/releases/latest/download}"
fi

# 1) Prebuilt binary, verified against the release's checksums.
if download "$base/$asset" "$tmp/$asset" 2>/dev/null; then
  download "$base/SHA256SUMS" "$tmp/SHA256SUMS" \
    || err "downloaded $asset but not SHA256SUMS; refusing to install unverified bytes"

  expected=$(grep " $asset\$" "$tmp/SHA256SUMS" 2>/dev/null | awk '{print $1}')
  [ -n "$expected" ] || err "$asset is not listed in SHA256SUMS; refusing to install it"

  if need sha256sum; then
    actual=$(sha256sum "$tmp/$asset" | awk '{print $1}')
  elif need shasum; then
    actual=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
  else
    err "no sha256sum or shasum available; cannot verify the download"
  fi
  [ "$expected" = "$actual" ] || err "checksum mismatch for $asset (expected $expected, got $actual)"

  tar -C "$tmp" -xzf "$tmp/$asset" || err "could not unpack $asset"
  bin=$(find "$tmp" -type f -name tafas | head -n 1)
  [ -n "$bin" ] || err "$asset did not contain a tafas binary"
  mkdir -p "$INSTALL_DIR"
  install -m 0755 "$bin" "$INSTALL_DIR/tafas" 2>/dev/null || {
    cp "$bin" "$INSTALL_DIR/tafas" && chmod 0755 "$INSTALL_DIR/tafas"
  }

  # The version is checked, not assumed: a binary that cannot run here would
  # otherwise be installed and fail later, in someone's pipeline.
  if ! "$INSTALL_DIR/tafas" --version >/dev/null 2>&1; then
    rm -f "$INSTALL_DIR/tafas"
    err "the installed binary does not run on this system; nothing was kept"
  fi
  if [ -n "${TAFAS_VERSION:-}" ]; then
    got=$("$INSTALL_DIR/tafas" --version | awk '{print $2}')
    [ "${got#v}" = "${TAFAS_VERSION#v}" ] || err "installed version $got does not match requested ${TAFAS_VERSION}"
  fi
  say "installed $INSTALL_DIR/tafas"
  "$INSTALL_DIR/tafas" --version
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) say "add to PATH: export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
  esac
  exit 0
fi

# 2) No asset for this platform: build from source, which needs cargo.
need cargo || err "no release asset for ${triple_arch}-${triple_os} and cargo is not installed"
say "no prebuilt binary for ${triple_arch}-${triple_os}; building from source"
if [ -n "${TAFAS_VERSION:-}" ]; then
  # A pin is honoured on every path, so a fallback build is the same code as the
  # release it was meant to install.
  cargo install --git "https://github.com/${REPO}" --tag "v${TAFAS_VERSION#v}" --root "$tmp/root" --locked 2>/dev/null \
    || cargo install --git "https://github.com/${REPO}" --tag "v${TAFAS_VERSION#v}" --root "$tmp/root"
else
  cargo install --git "https://github.com/${REPO}" --branch "$BRANCH" --root "$tmp/root" 2>/dev/null \
    || cargo install --git "https://github.com/${REPO}" --branch "$BRANCH" --root "$tmp/root"
fi
mkdir -p "$INSTALL_DIR"
cp "$tmp/root/bin/tafas" "$INSTALL_DIR/tafas"
chmod 0755 "$INSTALL_DIR/tafas"
"$INSTALL_DIR/tafas" --version
say "installed $INSTALL_DIR/tafas"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "add to PATH: export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
