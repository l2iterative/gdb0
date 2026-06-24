#!/usr/bin/env bash
# Run past _start into __start, then far enough to hit an ecall (or the known fault).
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
    exit 1
  fi
  sleep 0.1
done

"$gdb_bin" -nx -q -batch \
  -ex 'set pagination off' \
  -ex 'set confirm off' \
  -ex 'set print asm-demangle on' \
  -ex 'set architecture riscv:rv32' \
  -ex 'file code' \
  -ex 'target remote 127.0.0.1:9000' \
  -ex 'b *0x201cd4' \
  -ex 'continue' \
  -ex 'info registers pc a0 a7 t0 t6' \
  -ex 'x/4i $pc' \
  -ex 'b *0x200f00' \
  -ex 'continue' \
  -ex 'info registers pc a0 a7 t0 t6' \
  -ex 'x/8i $pc' \
  -ex 'detach' \
  >"$gdb_log" 2>&1 || true

echo "=== gdb log ==="
cat "$gdb_log"
echo
echo "=== server log ==="
cat "$server_log"

kill "$server_pid" >/dev/null 2>&1 || true
wait "$server_pid" >/dev/null 2>&1 || true
server_pid=""
