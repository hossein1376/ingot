#!/usr/bin/env bash
# Compare Ingot against C (-O0 and -O2) on the same kernels.
# On an x86 host this runs under qemu-user: absolute times are emulated and
# only the relative ordering is meaningful. Run natively on an M4 Pro for
# real numbers (use --target macos-arm64 and clang there).
set -euo pipefail
cd "$(dirname "$0")"
INGOT=${INGOT:-../ingot}
CC=${INGOT_CC:-zig cc}
TARGET=${TARGET:-linux-arm64}
if [ "$TARGET" = linux-arm64 ]; then CFLAGS="-target aarch64-linux-musl -static"; else CFLAGS="-arch arm64"; fi
RUN=""
if [ "$(uname -m)" != aarch64 ] && [ "$(uname -m)" != arm64 ]; then RUN=${INGOT_RUNNER:-qemu-aarch64}; fi
mkdir -p out
t() { local s e; s=$EPOCHREALTIME; "$@" >/dev/null; e=$EPOCHREALTIME; awk -v a="$s" -v b="$e" 'BEGIN{printf "%.2f", b-a}'; }
for k in collatz dot; do
  $INGOT build --target "$TARGET" "$k.ig" -o "out/$k-ingot"
  $CC $CFLAGS -O0 "$k.c" -o "out/$k-c-O0"
  $CC $CFLAGS -O2 "$k.c" -o "out/$k-c-O2"
  [ "$($RUN out/$k-ingot)" = "$($RUN out/$k-c-O2)" ] || { echo "$k: output mismatch"; exit 1; }
  printf "%-8s ingot %6ss   c-O0 %6ss   c-O2 %6ss\n" "$k" "$(t $RUN out/$k-ingot)" "$(t $RUN out/$k-c-O0)" "$(t $RUN out/$k-c-O2)"
done
