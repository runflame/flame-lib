#!/usr/bin/env bash
# Builds FlameWallet for Android: one libflamewallet_ffi.so per ABI, laid
# out as `jniLibs/<abi>/`, and the UniFFI-generated Kotlin that loads it.
# Needs the NDK (ANDROID_NDK_HOME, or found by cargo-ndk under ANDROID_HOME)
# and `cargo install cargo-ndk`. The Kotlin needs `net.java.dev.jna:jna`
# (`@aar`) on the app's classpath.
#
#   flamewallet-ffi/scripts/build-android.sh          # into target/flamewallet-ffi/android
#   ABIS="arm64-v8a" flamewallet-ffi/scripts/build-android.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${OUT:-$ROOT/target/flamewallet-ffi/android}"
PROFILE="${PROFILE:-release-ffi}"
API="${ANDROID_API:-24}"
read -r -a ABIS <<<"${ABIS:-arm64-v8a armeabi-v7a x86_64}"

triple() {
  case "$1" in
    arm64-v8a) echo aarch64-linux-android ;;
    armeabi-v7a) echo armv7-linux-androideabi ;;
    x86_64) echo x86_64-linux-android ;;
    x86) echo i686-linux-android ;;
    *) echo "unknown ABI $1" >&2; exit 1 ;;
  esac
}

command -v cargo-ndk >/dev/null || { echo "cargo-ndk missing: cargo install cargo-ndk" >&2; exit 1; }

cd "$ROOT"
TARGET_ARGS=()
for abi in "${ABIS[@]}"; do
  rustup target add "$(triple "$abi")"
  TARGET_ARGS+=(-t "$abi")
done

# Android 15 devices may use 16 KB pages, and Google Play requires 64-bit
# libraries aligned for them; NDK r27 still defaults to 4 KB.
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384"
export CARGO_TARGET_X86_64_LINUX_ANDROID_RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384"

# Only what this script writes: OUT may be an app's own source directory.
rm -rf "$OUT/jniLibs" "$OUT/kotlin"
cargo ndk --platform "$API" "${TARGET_ARGS[@]}" -o "$OUT/jniLibs" \
  build -p flamewallet-ffi --profile "$PROFILE"

# Bindings come from an unstripped host build, as for Apple.
cargo build -p flamewallet-ffi
case "$(uname -s)" in
  Darwin) HOST_LIB="target/debug/libflamewallet_ffi.dylib" ;;
  *) HOST_LIB="target/debug/libflamewallet_ffi.so" ;;
esac
cargo run -q -p flamewallet-ffi --features cli --bin uniffi-bindgen -- \
  generate --library "$HOST_LIB" --language kotlin --no-format --out-dir "$OUT/kotlin"

echo "jniLibs: $OUT/jniLibs"
echo "Kotlin:  $OUT/kotlin"
