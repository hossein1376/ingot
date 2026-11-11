#!/usr/bin/env bash
# Sets up an assembler/linker (zig cc) and, on non-arm64 Linux hosts, qemu-user
# so that Ingot programs can be built for both targets and run for linux-arm64.
# On an Apple-silicon Mac none of this is needed: Xcode's clang is used directly.
set -euo pipefail
PREFIX=${PREFIX:-$HOME/.ingot-toolchain}
mkdir -p "$PREFIX/bin"
if ! command -v zig >/dev/null; then
  python3 -m venv "$PREFIX/venv"
  "$PREFIX/venv/bin/pip" install -q ziglang
  printf '#!/bin/sh\nexec %s -m ziglang "$@"\n' "$PREFIX/venv/bin/python" > "$PREFIX/bin/zig"
  chmod +x "$PREFIX/bin/zig"
fi
case "$(uname -m)" in
  aarch64|arm64) ;;
  *)
    if ! command -v qemu-aarch64 >/dev/null; then
      curl -fsSL -o "$PREFIX/bin/qemu-aarch64" \
        https://github.com/multiarch/qemu-user-static/releases/download/v7.2.0-1/qemu-aarch64-static
      chmod +x "$PREFIX/bin/qemu-aarch64"
    fi ;;
esac
echo "export PATH=$PREFIX/bin:\$PATH"
