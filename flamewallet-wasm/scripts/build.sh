#!/usr/bin/env bash
# Builds @runflame/wallet-wasm: the module, its JS glue and TypeScript,
# laid out as an npm package. One `--target web` build serves every host:
# a bundler (Vite) or a Worker fetches the `.wasm` by URL, and Node passes
# its bytes to `initSync` (see flamewallet-wasm.md).
#
#   flamewallet-wasm/scripts/build.sh            # into target/wallet-wasm/pkg
#   OUT=/some/dir flamewallet-wasm/scripts/build.sh
#   RELEASE_VERSION=0.3.0-rc.1 flamewallet-wasm/scripts/build.sh
#
# The package takes RELEASE_VERSION when given (the release tag's version),
# else the crate's; the two must agree on X.Y.Z.
#
# Needs `wasm-bindgen` at exactly the version in Cargo.lock:
#   cargo +1.90.0 install wasm-bindgen-cli --version <that version> --locked
# and optionally binaryen's `wasm-opt`, which is used when present.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${OUT:-$ROOT/target/wallet-wasm/pkg}"
PROFILE="${PROFILE:-release-ffi}"
TARGET="wasm32-unknown-unknown"

cd "$ROOT"
LOCKED="$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"$/\1/p')"
FOUND="$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')"
if [ "$LOCKED" != "$FOUND" ]; then
  echo "wasm-bindgen ${FOUND:-missing}, Cargo.lock has $LOCKED:" >&2
  echo "  cargo +1.90.0 install wasm-bindgen-cli --version $LOCKED --locked" >&2
  exit 1
fi

rustup target add "$TARGET"
# FLAME_RUSTFLAGS as in the Apple and Android scripts: CI remaps paths through it.
RUSTFLAGS="${FLAME_RUSTFLAGS:-}" cargo build --locked -p flamewallet-wasm --profile "$PROFILE" --target "$TARGET"

rm -rf "$OUT"
wasm-bindgen --target web --out-dir "$OUT" \
  "target/$TARGET/$PROFILE/flamewallet_wasm.wasm"

if command -v wasm-opt >/dev/null; then
  wasm-opt -O3 --enable-bulk-memory --enable-nontrapping-float-to-int \
    "$OUT/flamewallet_wasm_bg.wasm" -o "$OUT/flamewallet_wasm_bg.wasm"
else
  echo "wasm-opt not found; the module is unoptimised by binaryen" >&2
fi

CRATE_VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' flamewallet-wasm/Cargo.toml | head -1)"
VERSION="${RELEASE_VERSION:-$CRATE_VERSION}"
if [ "${VERSION%%-*}" != "${CRATE_VERSION%%[-+]*}" ]; then
  echo "RELEASE_VERSION $VERSION, but flamewallet-wasm/Cargo.toml is $CRATE_VERSION" >&2
  exit 1
fi
cat >"$OUT/package.json" <<JSON
{
  "name": "@runflame/wallet-wasm",
  "version": "$VERSION",
  "description": "Flame wallet keys and confidential transfers, compiled to WebAssembly",
  "license": "Apache-2.0",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/runflame/flame-lib.git",
    "directory": "flamewallet-wasm"
  },
  "type": "module",
  "main": "./flamewallet_wasm.js",
  "types": "./flamewallet_wasm.d.ts",
  "exports": {
    ".": {
      "types": "./flamewallet_wasm.d.ts",
      "default": "./flamewallet_wasm.js"
    },
    "./flamewallet_wasm_bg.wasm": "./flamewallet_wasm_bg.wasm"
  },
  "files": [
    "flamewallet_wasm.js",
    "flamewallet_wasm.d.ts",
    "flamewallet_wasm_bg.wasm",
    "flamewallet_wasm_bg.wasm.d.ts",
    "LICENSE.txt",
    "sbom.cdx.json"
  ],
  "sideEffects": false
}
JSON
cp LICENSE.txt "$OUT/"

ls -la "$OUT"
echo "npm package: $OUT"
