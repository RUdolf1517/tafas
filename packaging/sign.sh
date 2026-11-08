#!/bin/sh
# Signs an artifact directory: one signature per file, one bundle, and a copy of
# the public key so a reader is never left guessing which key to check against.
#
#   packaging/sign.sh <artifacts-dir> <secret-key> <public-key>
#
# `SHA256SUMS` keeps its own signature (`SHA256SUMS.minisig`) — that is the one a
# reader checks first, and it answers for every file in the directory at once.
# Each artifact gets a detached signature beside it as well, because a mirror may
# hand out one file and not the collection, and a checksum file cannot vouch for
# itself.
#
# The key is created without a passphrase: minisign cannot read one from a pipe,
# and a release runs unattended. It lives in the secret store and nowhere else;
# the public half is committed as `packaging/minisign.pub`.
set -eu

dir="${1:?usage: sign.sh <artifacts-dir> <secret-key> <public-key>}"
secret="${2:?usage: sign.sh <artifacts-dir> <secret-key> <public-key>}"
public="${3:?usage: sign.sh <artifacts-dir> <secret-key> <public-key>}"

[ -f "$secret" ] || { printf 'sign: %s is not a key\n' "$secret" >&2; exit 1; }
[ -f "$public" ] || { printf 'sign: %s is not a public key\n' "$public" >&2; exit 1; }

cd "$dir"
[ -f SHA256SUMS ] || { printf 'sign: %s has no SHA256SUMS to sign\n' "$dir" >&2; exit 1; }

# Every artifact except the signature files themselves and the checksum file,
# which is signed last and separately. Flags start with `-`, so they are skipped
# by the same test that skips directories.
for file in *; do
  [ -f "$file" ] || continue
  case "$file" in
    SHA256SUMS | *.minisig) continue ;;
  esac
  minisign -S -s "$secret" -m "$file" -x "$file.minisig"
done

minisign -S -s "$secret" -m SHA256SUMS -x SHA256SUMS.minisig
cp "$public" minisign.pub

# Verify what was just written, against the public key, from the directory a
# reader would unpack into. A signature that does not verify is not published.
minisign -Vm SHA256SUMS -x SHA256SUMS.minisig -p minisign.pub >/dev/null
for file in *.minisig; do
  case "$file" in
    SHA256SUMS.minisig) continue ;;
  esac
  minisign -Vm "${file%.minisig}" -x "$file" -p minisign.pub >/dev/null
done

printf 'sign: signed %s files in %s\n' "$(ls -- *.minisig | wc -l)" "$dir"
