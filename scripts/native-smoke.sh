#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

old_log="$(mktemp)"
loop_log="$(mktemp)"

cleanup() {
  rm -f "$old_log" "$loop_log"
}
trap cleanup EXIT

cargo run --quiet -- --native-smoke --session-limit 1048576 >"$old_log" 2>&1
cat "$old_log"

grep -q 'native executor: constructed' "$old_log"
grep -q 'native smoke status: execution-error' "$old_log"
grep -q 'Illegal trap in machine mode' "$old_log"

loop_bin="$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" \
  -path '*/risc0-zkvm-5.0.0-rc.1/examples/loop.bin' \
  -type f \
  -print \
  -quit)"

if [[ -z "$loop_bin" ]]; then
  echo
  echo "skipping current loop.bin smoke; risc0-zkvm example ELF is not in the Cargo cache"
  exit 0
fi

echo
cargo run --quiet -- \
  --native-smoke \
  --code "$loop_bin" \
  --session-limit 1048576 \
  >"$loop_log" 2>&1
cat "$loop_log"

grep -q 'native executor: constructed' "$loop_log"
grep -q 'native smoke status: completed' "$loop_log"
grep -q 'exit_code: Halted(0)' "$loop_log"
