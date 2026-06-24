# RISC Zero VM Upgrade Notes

This pass compared the debugger against RISC Zero `main` at `e459039c`
(`2026-06-22`) and the published `risc0-core` / `risc0-zkp`
`5.0.0-rc.1` crates.

Relevant upstream files:

- `risc0/zkvm/platform/src/memory.rs`
- `risc0/zkvm/platform/src/syscall.rs`
- `risc0/circuit/rv32im/src/execute/platform.rs`
- `risc0/circuit/rv32im/src/execute/r0vm.rs`
- `risc0/circuit/rv32im/src/execute/sha2.rs`
- `risc0/circuit/rv32im/src/execute/poseidon2.rs`
- `risc0/circuit/rv32im/src/execute/bigint.rs`

## Covered In This Debugger

- Guest memory bounds now match `risc0-zkvm-platform`:
  `0x0000_4000..0xc000_0000`, with range checks that reject accesses
  crossing the end of memory.
- Ecall constants include current `USER`, `BIGINT2`, `POSEIDON2`, `SPLIT`,
  software syscall IDs, Keccak modes, and machine host ecall IDs.
- Software syscalls are classified by current `t6` IDs when available, while
  still accepting legacy syscall-name strings used by older guests.
- Current-style host ecalls through `a7` are supported for terminate, read,
  write, SHA2, and Poseidon2.
- Direct `t0 = POSEIDON2` is also supported, matching current platform guest
  code. It uses RISC Zero's current Poseidon2 Baby Bear implementation.
- Direct `t0 = BIGINT2` and machine `a7 = HOST_ECALL_BIGINT` both route through
  a BIBC decoder/evaluator ported from the current RV32IM executor. This covers
  debugger-visible guest memory side effects for nondeterministic programs,
  including the current arena-register calling convention and the executor's
  `t2 - 1 word` verify-region read.
- Keccak permutation, pipe fds, short reads, current cycle-count hi/lo returns,
  current two-word return buffers, and current log formatting are covered by
  focused unit tests.
- `scripts/verify-syscalls.sh` runs a syscall-focused test target. The tests
  compare local numeric syscall IDs, `nr::SYS_*` names, guest ecall constants,
  host ecall constants, file descriptors, `MAX_IO_BYTES`, Keccak modes, and
  Poseidon2 flags directly against `risc0-zkvm-platform` /
  `risc0-circuit-rv32im` `5.0.0-rc.1`.

## Upstream Executor Path

RISC Zero now ships `risc0_circuit_rv32im::execute::gdb::Debugger`, but it is
constructed around the public `Executor` type, not the old `rrs-lib` hart used
here. Moving to that path would mean creating a current `MemoryImage`, wiring
the current `Syscall` trait, and replacing the simulator state that all local
GDB read/write/breakpoint code currently mutates directly.

That is the likely route to a truly current debugger, but it is a larger
executor replacement rather than a local syscall compatibility patch.

## Native Executor Bridge Added

This repo now has an explicit native path in `src/vm/native.rs`:

- Raw user ELFs are wrapped as current `risc0_binfmt::ProgramBinary` values
  with `risc0_zkos_v1compat::V1COMPAT_ELF`.
- `--native-smoke` constructs `risc0_zkvm::ExecutorImpl` and runs a bounded
  execution through RISC Zero's current host syscall table.
- `--native-gdb` constructs the same executor and hands control to RISC Zero's
  native `run_with_debugger()` path.
- `.cargo/config.toml` sets `RISC0_SKIP_BUILD_KERNELS=1` so execution/debugger
  builds do not require the Apple Metal toolchain just because `ExecutorImpl`
  is exported behind the `prove` feature.

Validation found an important compatibility boundary:

- A current packaged RISC Zero example ELF
  (`risc0-zkvm-5.0.0-rc.1/examples/loop.bin`) runs through the new native
  bridge and halts cleanly with `Halted(0)`.
- The checked-in `code` ELF is old enough that, when wrapped with the current
  v1compat kernel, it enters the official executor but fails in machine mode:
  `Execution failed at program counter 0xc0000a68: Illegal trap in machine
  mode`.

So the native bridge is compatible with current RISC Zero ELFs/syscalls, but
the historical sample ELF is not transparently compatible with today's
v1compat kernel. Keeping the standalone compatibility VM remains useful for
debugging that artifact.

## Still Not A Full Current RISC Zero VM

The current RV32IM executor has a machine/user mode split, kernel and machine
regions, trap dispatch, register-memory backing around `0xffff_0000`, and
host ecall dispatch from machine mode. This debugger still runs a flat
`rrs-lib` hart and intercepts ecalls directly.

The `BIGINT2` support here executes the current BIBC nondeterministic program
well enough for debugger state changes, but it does not produce the executor's
proof witness chunks or model the current machine/user kernel transition
cycle-exactly.

Custom `USER` syscalls, full receipt verification side effects, recursive proof
coprocessor behavior, and cycle-exact current segment accounting are also out
of scope for this standalone compatibility pass.
