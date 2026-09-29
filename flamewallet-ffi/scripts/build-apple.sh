#!/usr/bin/env bash
# Builds FlameWallet as a Swift package: an xcframework of the static
# library for device, simulator and macOS (for SwiftUI previews and
# `swift test` on a Mac), and the UniFFI-generated Swift around it.
#
#   flamewallet-ffi/scripts/build-apple.sh            # into target/flamewallet-ffi/apple
#   OUT=/some/dir flamewallet-ffi/scripts/build-apple.sh
#   SIM_TARGETS="aarch64-apple-ios-sim" ...           # skip the Intel simulator
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${OUT:-$ROOT/target/flamewallet-ffi/apple}"
PROFILE="${PROFILE:-release-ffi}"
DEVICE_TARGET="aarch64-apple-ios"
MACOS_TARGET="aarch64-apple-darwin"
read -r -a SIM_TARGETS <<<"${SIM_TARGETS:-aarch64-apple-ios-sim x86_64-apple-ios}"
LIB="libflamewallet_ffi.a"

cd "$ROOT"
rustup target add "$DEVICE_TARGET" "$MACOS_TARGET" "${SIM_TARGETS[@]}"

for target in "$DEVICE_TARGET" "$MACOS_TARGET" "${SIM_TARGETS[@]}"; do
  cargo build -p flamewallet-ffi --profile "$PROFILE" --target "$target"
done

# Bindings come from an unstripped host build: bindgen reads the metadata
# UniFFI embeds in the library, and the shipped profile strips symbols.
cargo build -p flamewallet-ffi
PKG="$OUT/FlameWallet"
rm -rf "$PKG" "$OUT/work"
mkdir -p "$PKG/Sources/FlameWallet" "$OUT/work/headers" "$OUT/work/sim"
cargo run -q -p flamewallet-ffi --features cli --bin uniffi-bindgen -- \
  generate --library "target/debug/libflamewallet_ffi.dylib" \
  --language swift --out-dir "$OUT/work/swift"
mv "$OUT/work/swift/FlameWallet.swift" "$PKG/Sources/FlameWallet/"
cp "$OUT/work/swift/FlameWalletFFI.h" "$OUT/work/headers/"
cp "$OUT/work/swift/FlameWalletFFI.modulemap" "$OUT/work/headers/module.modulemap"

# One slice per platform: the simulator architectures are fused first.
SIM_LIBS=()
for target in "${SIM_TARGETS[@]}"; do
  SIM_LIBS+=("target/$target/$PROFILE/$LIB")
done
lipo -create "${SIM_LIBS[@]}" -output "$OUT/work/sim/$LIB"

xcodebuild -create-xcframework \
  -library "target/$DEVICE_TARGET/$PROFILE/$LIB" -headers "$OUT/work/headers" \
  -library "$OUT/work/sim/$LIB" -headers "$OUT/work/headers" \
  -library "target/$MACOS_TARGET/$PROFILE/$LIB" -headers "$OUT/work/headers" \
  -output "$PKG/FlameWalletFFI.xcframework"

cat >"$PKG/Package.swift" <<'SWIFT'
// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "FlameWallet",
    platforms: [.iOS(.v13), .macOS(.v11)],
    products: [.library(name: "FlameWallet", targets: ["FlameWallet"])],
    targets: [
        .binaryTarget(name: "FlameWalletFFI", path: "FlameWalletFFI.xcframework"),
        .target(name: "FlameWallet", dependencies: ["FlameWalletFFI"]),
    ]
)
SWIFT

rm -rf "$OUT/work"
echo "Swift package: $PKG"
