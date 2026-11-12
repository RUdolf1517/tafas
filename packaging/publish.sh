#!/bin/sh
# Publish the crate to crates.io, from the owner's machine.
#
# `docs/releases.md` §4.1 says why this is the one step of a release that needs a
# credential, and this script is that step written down so it is the same every
# time. It refuses to run in the three situations that produce a broken archive
# rather than a rejected upload — which is worse, because a rejected upload can
# be retried and a published archive cannot be replaced.
#
#   ./packaging/publish.sh          # package, verify, upload
#   ./packaging/publish.sh --check  # package and verify only
#
# What it needs: cargo, a login (`cargo login` once), and a clean tree.

set -eu

check_only=false
case "${1:-}" in
  --check) check_only=true ;;
  "") ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

fail() {
  echo "publish: $*" >&2
  exit 1
}

step() {
  printf '\n== %s\n' "$*"
}

# 1. The tree has to be clean. A version published from a dirty tree is a
#    commit nobody can check out again, and the two archives — the one on
#    crates.io and the one this commit builds — are then different files with the
#    same name.
step "the tree is clean"
if [ -n "$(git status --porcelain)" ]; then
  git status --short
  fail "the tree is dirty; a published crate has to be a commit"
fi
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
[ -n "$version" ] || fail "Cargo.toml has no version"
commit="$(git rev-parse --short HEAD)"
echo "version $version at $commit"

# 2. The tag has to exist and point here: the crate and the release have to be
#    the same tree, or a reader installing one gets something the other never had.
step "the tag v$version is this commit"
if git rev-parse -q --verify "refs/tags/v${version}" >/dev/null; then
  tagged="$(git rev-parse --short "refs/tags/v${version}^{commit}")"
  [ "$tagged" = "$commit" ] || fail "tag v$version is $tagged, not $commit"
  echo "v$version -> $commit"
else
  echo "no tag v$version yet; fine for a first upload, and 'git tag v$version' after this"
fi

# 3. The crate is not already there. `cargo publish` would say so, and late; the
#    answer also says whether this is the first upload or a re-run.
step "crates.io does not already have $version"
# `|| true` on purpose: a machine with no network must still be able to package
# and verify, and the answer is only used to stop a re-publish early.
code="$(curl -s -o /dev/null -w '%{http_code}' \
  -A "tafas-publish (https://github.com/RUdolf1517/tafas)" \
  "https://crates.io/api/v1/crates/tafas/$version" || true)"
code="${code:-none}"
case "$code" in
  200)
    echo "tafas $version is published already"
    if [ "$check_only" = true ]; then
      exit 0
    fi
    fail "this version is on crates.io; bump Cargo.toml — a version cannot be replaced, only yanked"
    ;;
  404) echo "not published yet (HTTP 404)" ;;
  *) echo "could not ask crates.io (HTTP ${code}); continuing, and cargo will refuse if it is taken" ;;
esac

# 4. What the archive will hold. `cargo package --list` is the file list itself,
#    so this is the check and not a description of the check.
step "the archive holds the source, and only the source"
cargo package --list | sort > /tmp/tafas-package-files.$$
trap 'rm -f /tmp/tafas-package-files.$$' EXIT
for required in Cargo.toml LICENSE README.md CHANGELOG.md src/lib.rs src/main.rs; do
  grep -qx "$required" /tmp/tafas-package-files.$$ ||
    fail "$required is missing from the archive"
done
for forbidden in '^packaging/' '^\.github/' '^fuzz/' '^tests/' '^Formula/' '^target/'; do
  if grep -qE "$forbidden" /tmp/tafas-package-files.$$; then
    fail "$forbidden is in the archive and should not be"
  fi
done
echo "$(wc -l < /tmp/tafas-package-files.$$ | tr -d ' ') files"

# 4b. And the changelog has to describe this version. The archive carries
#     CHANGELOG.md, so a release whose section is still `Unreleased` ships a
#     document that never mentions the number on the front of it — 0.2.1 was
#     tagged while the work it was meant to carry was on another commit, and a
#     check that the number and the notes agree is what notices next time.
step "CHANGELOG.md has a section for $version"
grep -q "^## \\[$version\\]" CHANGELOG.md ||
  fail "CHANGELOG.md has no \"## [$version]\" section; write the notes before spending the number"

# 5. Build the archive the way `cargo install tafas` will: unpack the .crate and
#    compile it from there. A file the build needs but the package forgot fails
#    here, at the last moment it can be fixed, instead of in a user's terminal.
step "the unpacked crate builds and runs"
cargo package
work="$(mktemp -d)"
trap 'rm -rf "$work" /tmp/tafas-package-files.$$' EXIT
archive="target/package/tafas-${version}.crate"
[ -f "$archive" ] || fail "$archive was not built"
tar -xzf "$archive" -C "$work"
(
  cd "$work/tafas-$version"
  cargo build --release --quiet
  ./target/release/tafas --version
)

step "ready"
if [ "$check_only" = true ]; then
  echo "--check: nothing uploaded"
  exit 0
fi

# 6. The irreversible step, last and alone.
printf '\npublish tafas %s to crates.io? this cannot be undone. [y/N] ' "$version"
read -r answer
case "$answer" in
  y | Y | yes) ;;
  *) echo "nothing uploaded"; exit 0 ;;
esac
cargo publish
echo
echo "published tafas $version."
echo "tag it if you have not:  git tag v$version && git push origin v$version"
