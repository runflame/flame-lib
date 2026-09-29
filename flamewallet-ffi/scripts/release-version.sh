#!/usr/bin/env bash
# Checks a release tag against the crates it releases and prints the npm
# dist-tag the release goes out under.
#
#   flamewallet-ffi/scripts/release-version.sh flamewallet-v0.3.0        # → latest
#   flamewallet-ffi/scripts/release-version.sh flamewallet-v0.3.0-rc.1   # → next
#
# One version names the whole set — flamewallet-ffi, flamewallet-wasm and
# both npm packages built from them. The packages carry the tag's version
# whole; the crates carry only its X.Y.Z, so `-rc.N` lives in the tag alone
# and a tag whose X.Y.Z disagrees with either Cargo.toml is refused.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TAG="${1:?usage: release-version.sh flamewallet-v<version>}"
PREFIX="flamewallet-v"

case "$TAG" in
  "$PREFIX"*) VERSION="${TAG#"$PREFIX"}" ;;
  *) echo "tag $TAG does not start with $PREFIX" >&2; exit 1 ;;
esac
if ! printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'; then
  echo "tag $TAG does not carry a semver version" >&2
  exit 1
fi

for crate in flamewallet-ffi flamewallet-wasm; do
  crate_version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/$crate/Cargo.toml" | head -1)"
  if [ "${crate_version%%[-+]*}" != "${VERSION%%-*}" ]; then
    echo "tag $TAG, but $crate/Cargo.toml is $crate_version" >&2
    exit 1
  fi
done

# A prerelease must never become what `npm install` picks by default.
case "$VERSION" in
  *-*) echo next ;;
  *) echo latest ;;
esac
