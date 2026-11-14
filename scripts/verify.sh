#!/usr/bin/env bash
# Gate for the Ingot compiler workspace. Run it from anywhere; it cds to the repo
# root. The last line is `== verify: PASS` or `== verify: FAIL`, and the exit status
# is 0 only for PASS.
#
# Gates 1 to 3 and 5 decide the result. Gate 4 (the conformance suite) is printed
# for information only: while tests are still failing it exits non-zero, which is
# expected. The ratchet test in crates/driver/tests/conformance.rs is the real gate
# for the suite and runs under gate 3.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1
fail=0
gate() { if [ "$1" -ne 0 ]; then echo "   FAILED: $2"; fail=1; else echo "   ok"; fi; }

echo "== base $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"

echo "== 1 fmt";        cargo fmt --all --check >/dev/null 2>&1; gate $? "cargo fmt --all --check"

echo "== 2 clippy";     cargo clippy --workspace --all-targets --release -q -- -D warnings 2>&1 | tail -30
                        gate "${PIPESTATUS[0]}" "clippy -D warnings"

echo "== 3 cargo test"; out=$(cargo test --workspace --release 2>&1); rc=$?
                        echo "$out" | grep -E '^test result|FAILED|panicked|^error' | sort | uniq -c
                        if [ $rc -ne 0 ]; then echo "$out" | tail -60; fi
                        gate $rc "cargo test --workspace"

echo "== 4 conformance (informational; the ratchet test is the gate)"
cargo build --workspace --release -q; build_rc=$?
if [ $build_rc -ne 0 ]; then
  echo "   FAILED: cargo build --workspace --release"; fail=1
else
  conf=$(./target/release/ingot conformance conformance 2>&1); conf_rc=$?
  echo "$conf" | tail -1
  echo "   exit $conf_rc (non-zero is expected while the suite has failing tests)"
fi

# 5: three runs of `ingot check --json` per conformance entry must be byte-identical.
# Rust's HashMap seeds differ per process, so any order leak shows up here.
# Timing fields ("ms") are zeroed first: tools.md §1 exempts them.
echo "== 5 determinism"
bad=0; n=0
for entry in conformance/*/*; do
  case "$entry" in *.fixed.ingot) continue;; *.ingot) ;; *) [ -d "$entry" ] || continue;; esac
  n=$((n+1))
  a=$(./target/release/ingot check --json "$entry" 2>&1 | sed -E 's/"ms":[0-9]+/"ms":0/g' | shasum)
  for _ in 1 2; do
    b=$(./target/release/ingot check --json "$entry" 2>&1 | sed -E 's/"ms":[0-9]+/"ms":0/g' | shasum)
    if [ "$a" != "$b" ]; then echo "   nondeterministic: $entry"; bad=1; break; fi
  done
done
echo "   $n entries checked"; gate $bad "determinism"

echo "== verify: $([ $fail -eq 0 ] && echo PASS || echo FAIL)"
exit $fail
