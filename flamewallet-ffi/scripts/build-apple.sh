#!/usr/bin/env bash
# Builds FlameWallet for Apple platforms: FlameWalletFFI.xcframework, a
# dynamic framework per platform — iOS device, iOS simulator (arm64 and
# x86_64 fused), and macOS for `swift test` and SwiftUI previews on a Mac —
# with the UniFFI-generated Swift, laid out as a Swift package.
#
#   flamewallet-ffi/scripts/build-apple.sh            # into target/flamewallet-ffi/apple
#   OUT=/some/dir flamewallet-ffi/scripts/build-apple.sh
#   SIM_TARGETS="aarch64-apple-ios-sim" ...           # skip the Intel simulator
#
# Dynamic, not static: a static library carries the whole of std and every
# dependency's objects unlinked — 37 MB for the device slice alone — while
# the linked dylib is a few megabytes, and it is what a package ships.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${OUT:-$ROOT/target/flamewallet-ffi/apple}"
PROFILE="${PROFILE:-release-ffi}"
DEVICE_TARGET="aarch64-apple-ios"
MACOS_TARGET="aarch64-apple-darwin"
read -r -a SIM_TARGETS <<<"${SIM_TARGETS:-aarch64-apple-ios-sim x86_64-apple-ios}"
NAME="FlameWalletFFI"
DYLIB="libflamewallet_ffi.dylib"
# What the framework's Info.plist promises, and what the linker is told.
IOS_MIN="15.1"
MACOS_MIN="11.0"
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/flamewallet-ffi/Cargo.toml" | head -1)"
# CFBundleShortVersionString and CFBundleVersion must be period-separated
# integers, or App Store Connect rejects the app that embeds the framework
# (ITMS-90060): a prerelease `0.3.0-rc.1` is stamped `0.3.0`.
BUNDLE_VERSION="${VERSION%%[-+]*}"

cd "$ROOT"
rustup target add "$DEVICE_TARGET" "$MACOS_TARGET" "${SIM_TARGETS[@]}"

# The install name is where the app's loader looks for the binary: inside
# the framework, found through the app's rpath. A macOS framework keeps its
# binary under Versions/A.
build() {
  local target="$1" install_name="$2"
  local var="CARGO_TARGET_$(echo "$target" | tr '[:lower:]-' '[:upper:]_')_RUSTFLAGS"
  # cargo reads the per-target variable only while RUSTFLAGS is unset, so
  # flags for every target (CI's path remapping) arrive as FLAME_RUSTFLAGS.
  env -u RUSTFLAGS "$var=${FLAME_RUSTFLAGS:-} -C link-arg=-Wl,-install_name,$install_name" \
    IPHONEOS_DEPLOYMENT_TARGET="$IOS_MIN" MACOSX_DEPLOYMENT_TARGET="$MACOS_MIN" \
    cargo build --locked -p flamewallet-ffi --lib --profile "$PROFILE" --target "$target"
}
for target in "$DEVICE_TARGET" "${SIM_TARGETS[@]}"; do
  build "$target" "@rpath/$NAME.framework/$NAME"
done
build "$MACOS_TARGET" "@rpath/$NAME.framework/Versions/A/$NAME"

# Bindings come from an unstripped host build: bindgen reads the metadata
# UniFFI embeds in the library, and the shipped profile strips symbols.
cargo build --locked -p flamewallet-ffi --lib
PKG="$OUT/FlameWallet"
WORK="$OUT/work"
rm -rf "$PKG" "$WORK"
mkdir -p "$PKG/Sources/FlameWallet" "$WORK/sim"
cargo run --locked -q -p flamewallet-ffi --features cli --bin uniffi-bindgen -- \
  generate --library "target/debug/$DYLIB" --language swift --out-dir "$WORK/swift"
mv "$WORK/swift/FlameWallet.swift" "$PKG/Sources/FlameWallet/"

plist() {
  local platform="$1" min_key="$2" min="$3"
  cat <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>$NAME</string>
  <key>CFBundleIdentifier</key><string>com.runflame.$NAME</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>$NAME</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>$BUNDLE_VERSION</string>
  <key>CFBundleVersion</key><string>$BUNDLE_VERSION</string>
  <key>CFBundleSupportedPlatforms</key><array><string>$platform</string></array>
  <key>$min_key</key><string>$min</string>
</dict>
</plist>
PLIST
}

# A framework module, so Swift finds the C declarations as `import FlameWalletFFI`.
modulemap() {
  printf 'framework module %s {\n    umbrella header "%s.h"\n    export *\n}\n' "$NAME" "$NAME"
}

# iOS frameworks are shallow: binary, Info.plist, Headers, Modules at the root.
ios_framework() {
  local dylib="$1" dir="$2" platform="$3"
  local fw="$dir/$NAME.framework"
  mkdir -p "$fw/Headers" "$fw/Modules"
  cp "$dylib" "$fw/$NAME"
  cp "$WORK/swift/$NAME.h" "$fw/Headers/"
  modulemap >"$fw/Modules/module.modulemap"
  plist "$platform" MinimumOSVersion "$IOS_MIN" >"$fw/Info.plist"
}

# macOS frameworks are versioned: everything under Versions/A, symlinked up.
macos_framework() {
  local dylib="$1" dir="$2"
  local fw="$dir/$NAME.framework"
  local v="$fw/Versions/A"
  mkdir -p "$v/Headers" "$v/Modules" "$v/Resources"
  cp "$dylib" "$v/$NAME"
  cp "$WORK/swift/$NAME.h" "$v/Headers/"
  modulemap >"$v/Modules/module.modulemap"
  plist MacOSX LSMinimumSystemVersion "$MACOS_MIN" >"$v/Resources/Info.plist"
  ln -s A "$fw/Versions/Current"
  for entry in "$NAME" Headers Modules Resources; do
    ln -s "Versions/Current/$entry" "$fw/$entry"
  done
}

SIM_LIBS=()
for target in "${SIM_TARGETS[@]}"; do
  SIM_LIBS+=("target/$target/$PROFILE/$DYLIB")
done
lipo -create "${SIM_LIBS[@]}" -output "$WORK/sim/$DYLIB"

ios_framework "target/$DEVICE_TARGET/$PROFILE/$DYLIB" "$WORK/ios" iPhoneOS
ios_framework "$WORK/sim/$DYLIB" "$WORK/ios-sim" iPhoneSimulator
macos_framework "target/$MACOS_TARGET/$PROFILE/$DYLIB" "$WORK/macos"

xcodebuild -create-xcframework \
  -framework "$WORK/ios/$NAME.framework" \
  -framework "$WORK/ios-sim/$NAME.framework" \
  -framework "$WORK/macos/$NAME.framework" \
  -output "$PKG/$NAME.xcframework"

cat >"$PKG/Package.swift" <<'SWIFT'
// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "FlameWallet",
    platforms: [.iOS("15.1"), .macOS(.v11)],
    products: [.library(name: "FlameWallet", targets: ["FlameWallet"])],
    targets: [
        .binaryTarget(name: "FlameWalletFFI", path: "FlameWalletFFI.xcframework"),
        .target(name: "FlameWallet", dependencies: ["FlameWalletFFI"]),
    ]
)
SWIFT

rm -rf "$WORK"
du -sh "$PKG/$NAME.xcframework"/*/
echo "Swift package: $PKG"
