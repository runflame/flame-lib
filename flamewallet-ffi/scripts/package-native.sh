#!/usr/bin/env bash
# Lays out @runflame/wallet-rn from what build-apple.sh and
# build-android.sh produced: a CocoaPods pod and an Android library project,
# both with the UniFFI bindings, and no React Native or Expo code at all.
# An Expo app's own module depends on it; `expo-module.config.json` is what
# makes Expo autolinking pick up the pod and the Gradle project.
#
#   flamewallet-ffi/scripts/build-apple.sh
#   flamewallet-ffi/scripts/build-android.sh
#   flamewallet-ffi/scripts/package-native.sh     # into target/wallet-rn/pkg
#
# The package takes RELEASE_VERSION when given (the release tag's version),
# else the crate's; the two must agree on X.Y.Z.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APPLE="${APPLE:-$ROOT/target/flamewallet-ffi/apple/FlameWallet}"
ANDROID="${ANDROID:-$ROOT/target/flamewallet-ffi/android}"
OUT="${OUT:-$ROOT/target/wallet-rn/pkg}"
CRATE_VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/flamewallet-ffi/Cargo.toml" | head -1)"
VERSION="${RELEASE_VERSION:-$CRATE_VERSION}"
if [ "${VERSION%%-*}" != "${CRATE_VERSION%%[-+]*}" ]; then
  echo "RELEASE_VERSION $VERSION, but flamewallet-ffi/Cargo.toml is $CRATE_VERSION" >&2
  exit 1
fi
KOTLIN_PACKAGE="$(sed -n 's/^package_name = "\(.*\)"$/\1/p' "$ROOT/flamewallet-ffi/uniffi.toml")"

[ -d "$APPLE/FlameWalletFFI.xcframework" ] || { echo "no $APPLE; run build-apple.sh" >&2; exit 1; }
[ -d "$ANDROID/jniLibs" ] || { echo "no $ANDROID; run build-android.sh" >&2; exit 1; }

# The slice folders are named after the architectures built into them, and
# build-apple.sh's SIM_TARGETS changes the simulator's (`ios-arm64-simulator`
# for Apple silicon alone), so each is found rather than assumed.
slice() {
  local pattern="$1" found=()
  for dir in "$APPLE"/FlameWalletFFI.xcframework/$pattern; do
    [ -d "$dir/FlameWalletFFI.framework" ] && found+=("$dir/FlameWalletFFI.framework")
  done
  if [ "${#found[@]}" -ne 1 ]; then
    echo "expected one $pattern slice in $APPLE/FlameWalletFFI.xcframework, found ${#found[@]}" >&2
    exit 1
  fi
  printf '%s\n' "${found[0]}"
}
DEVICE_SLICE="$(slice 'ios-arm64')"
SIM_SLICE="$(slice 'ios-*-simulator')"

rm -rf "$OUT"
mkdir -p "$OUT/ios" "$OUT/android/src/main"
# iOS slices only. The pod is iOS-only, and the macOS slice could not travel
# anyway: a macOS framework is built from symlinks, which npm drops from a
# tarball. It stays in the Swift package, for host tests.
xcodebuild -create-xcframework \
  -framework "$DEVICE_SLICE" \
  -framework "$SIM_SLICE" \
  -output "$OUT/ios/FlameWalletFFI.xcframework" >/dev/null
cp "$APPLE/Sources/FlameWallet/FlameWallet.swift" "$OUT/ios/"
cp -R "$ANDROID/jniLibs" "$OUT/android/src/main/"
cp -R "$ANDROID/kotlin" "$OUT/android/src/main/java"
cp "$ROOT/LICENSE.txt" "$OUT/LICENSE.txt"

cat >"$OUT/FlameWalletNative.podspec" <<RUBY
Pod::Spec.new do |s|
  s.name             = 'FlameWalletNative'
  # Swift code imports the bindings as \`FlameWallet\`.
  s.module_name      = 'FlameWallet'
  s.version          = '$VERSION'
  s.summary          = 'Flame wallet keys and confidential transfers, with UniFFI Swift bindings'
  s.homepage         = 'https://github.com/runflame/flame-lib'
  s.license          = { :type => 'Apache-2.0', :file => 'LICENSE.txt' }
  s.author           = 'Runflame'
  s.source           = { :git => 'https://github.com/runflame/flame-lib.git', :tag => 'flamewallet-v$VERSION' }
  s.platforms        = { :ios => '15.1' }
  s.swift_version    = '5.9'
  s.source_files     = 'ios/FlameWallet.swift'
  s.vendored_frameworks = 'ios/FlameWalletFFI.xcframework'
end
RUBY

cat >"$OUT/android/build.gradle" <<GRADLE
// The Rust library for each ABI, and the UniFFI Kotlin that loads it
// through JNA. Plugin versions come from the app that includes the project.
plugins {
  id 'com.android.library'
  id 'org.jetbrains.kotlin.android'
}

android {
  namespace '$KOTLIN_PACKAGE'
  compileSdk 35

  defaultConfig {
    minSdk 24
    versionName '$VERSION'
  }

  compileOptions {
    sourceCompatibility JavaVersion.VERSION_17
    targetCompatibility JavaVersion.VERSION_17
  }

  kotlinOptions {
    jvmTarget = '17'
  }
}

dependencies {
  // \`api\`: the generated classes expose JNA types to whoever calls them.
  api 'net.java.dev.jna:jna:5.15.0@aar'
}
GRADLE

cat >"$OUT/expo-module.config.json" <<'JSON'
{
  "platforms": ["apple", "android"]
}
JSON

cat >"$OUT/package.json" <<JSON
{
  "name": "@runflame/wallet-rn",
  "version": "$VERSION",
  "description": "Flame wallet keys and confidential transfers for iOS and Android: prebuilt libraries with UniFFI Swift and Kotlin bindings",
  "license": "Apache-2.0",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/runflame/flame-lib.git",
    "directory": "flamewallet-ffi"
  },
  "files": [
    "FlameWalletNative.podspec",
    "expo-module.config.json",
    "LICENSE.txt",
    "sbom.cdx.json",
    "ios",
    "android"
  ]
}
JSON

du -sh "$OUT"
echo "npm package: $OUT"
