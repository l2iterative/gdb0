#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

gdb_bin="${GDB:-riscv64-elf-gdb}"
if ! command -v "$gdb_bin" >/dev/null 2>&1; then
  echo "missing GDB binary: $gdb_bin" >&2
  exit 127
fi

cargo build >/dev/null

server_log="$(mktemp)"
gdb_log="$(mktemp)"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" >/dev/null 2>&1 || true
  fi
  rm -f "$server_log" "$gdb_log"
}
trap cleanup EXIT

target/debug/r0db >"$server_log" 2>&1 &
server_pid="$!"

for _ in $(seq 1 100); do
  if grep -q 'Waiting for a GDB connection' "$server_log"; then
    break
  fi
  if ! kill -0 "$server_pid" >/dev/null 2>&1; then
    cat "$server_log" >&2
    echo "debugger exited before accepting GDB" >&2
    exit 1
  fi
  sleep 0.1
done

if ! grep -q 'Waiting for a GDB connection' "$server_log"; then
  cat "$server_log" >&2
  echo "debugger did not start listening in time" >&2
  exit 1
fi

"$gdb_bin" -nx -q -batch \
  -ex 'set pagination off' \
  -ex 'set confirm off' \
  -ex 'set architecture riscv:rv32' \
  -ex 'file code' \
  -ex 'target remote 127.0.0.1:9000' \
  -ex 'info registers pc' \
  -ex 'si' \
  -ex 'info registers pc' \
  -ex 'x/i $pc' \
  -ex 'detach' \
  >"$gdb_log" 2>&1

cat "$gdb_log"

grep -q '0x201cc0 <_start+4>' "$gdb_log"
grep -q 'addi[[:space:]]\+gp,gp,-1212' "$gdb_log"

wait "$server_pid" || true
server_pid=""

echo
echo "server log:"
cat "$server_log"
