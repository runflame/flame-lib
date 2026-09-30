#!/usr/bin/env bash
# Prints the rustc flags that take the build machine's paths out of a
# binary — the checkout, the cargo home where every dependency's source
# sits, and the toolchain's sysroot, whose std sources panic locations
# name — so two builds of one commit on different machines can be compared
# byte for byte. The release workflow passes them as FLAME_RUSTFLAGS.
set -euo pipefail

WORKSPACE="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
# From the workspace, so rust-toolchain.toml picks the toolchain.
SYSROOT="$(cd "$WORKSPACE" && rustc --print sysroot)"
printf -- '--remap-path-prefix=%s=/flame-lib --remap-path-prefix=%s=/cargo --remap-path-prefix=%s=/rust\n' \
  "$WORKSPACE" "$CARGO_HOME" "$SYSROOT"
