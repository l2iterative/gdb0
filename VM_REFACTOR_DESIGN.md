# VM Refactor Design Notes

## Goal

Replace the `rrs_lib`-based flat hart with a trait-based VM abstraction,
allowing future swap to a cycle-exact, privilege-mode-aware execution
engine without rewriting the GDB layer.

## Original State Before This Refactor Slice

```
Debugger (debug/debugger.rs)
  └─ directly mutates Simulator.hart_state.registers[..]
  └─ directly calls Simulator.mem.borrow_mut()
  └─ GDB read/write hits a flat [u32; 32] register array
  └─ ecall dispatch is inline in Simulator.ecall()
```

The `Simulator` struct (`src/vm/simulator.rs:28`)
owns everything: `HartState`, `Memory`, I/O cursors, env, pipe FDs, session cycle
count. The `InstructionExecutor` from `rrs_lib` is constructed on-the-fly at
`simulator.rs:377` and calls `.step()` to run one instruction.

## What We Found In Upstream

Analyzed `risc0_circuit_rv32im::execute::gdb::Debugger` at commit
`e459039c` (RISC Zero main, 2026-06-22). The upstream debugger was an
incomplete prototype (no hardware watchpoints, no HostIo, broken interrupt
handling), but the **architecture** is sound:

```
Debugger --> Executor --> Risc0Context trait
                            |
                load_u32, store_u32,
                load_region, store_region,
                load_register, store_register,
                host_read, host_write,
                get_pc, set_pc,
                get_machine_mode, set_machine_mode,
                ecall_bigint, ecall_poseidon2,
                suspend, resume, on_terminate...

              Emulator.step(&mut EmuContext)
                            |
                load_register, store_register,
                load_memory, store_memory,
                ecall, mret, trap...
```

Key files from upstream:

- `risc0/circuit/rv32im/src/execute/gdb.rs` — the `Debugger` struct and GDB
  stub impls
- `risc0/circuit/rv32im/src/execute/executor.rs` — `Executor` struct,
  `Risc0Context` trait, `SegmentUpdate`, segment splitting
- `risc0/circuit/rv32im/src/execute/r0vm.rs` — `Risc0Machine`,
  `EmuContext` trait (instruction-level interface), ecall/trap dispatch,
  register-in-memory model
- `risc0/circuit/rv32im/src/execute/platform.rs` — address layout
  constants: `USER_REGS_ADDR = 0xffff_0080`,
  `MACHINE_REGS_ADDR = 0xffff_0000`,
  `KERNEL_START_ADDR = 0xc000_0000`, etc.

## Design Patterns To Adopt

### 1. Trait-based VM abstraction (highest priority)

Introduce a `VmContext` trait that the GDB layer talks to, instead of
reaching into `Simulator` fields directly:

```rust
pub trait VmContext {
    fn get_pc(&self) -> u32;
    fn set_pc(&mut self, pc: u32);
    fn get_machine_mode(&self) -> u32;
    fn set_machine_mode(&mut self, mode: u32);

    fn load_register(&self, idx: usize) -> u32;
    fn store_register(&mut self, idx: usize, word: u32);

    fn load_u32(&mut self, addr: u32) -> Option<u32>;
    fn store_u32(&mut self, addr: u32, word: u32) -> bool;
    fn load_region(&mut self, addr: u32, size: usize) -> Vec<u8>;
    fn store_region(&mut self, addr: u32, data: &[u8]) -> bool;

    fn step(&mut self) -> Result<(u32, Option<ExitCode>, usize)>;

    fn host_read(&mut self, fd: u32, buf: &mut [u8]) -> Result<u32>;
    fn host_write(&mut self, fd: u32, buf: &[u8]) -> Result<u32>;

    fn get_cycle_count(&self) -> u64;
}
```

The `Debugger` struct becomes:

```rust
pub struct Debugger<VM: VmContext> {
    pub elf: Vec<u8>,
    pub vm: VM,
    pub exec_mode: ExecMode,
    pub breakpoints: HashSet<u32>,
}
```

All `debug/readwrite.rs`, `debug/step.rs`, `debug/breakpoints.rs`
code would call methods on `self.vm` instead of `self.simulator.borrow_mut()`.
The `Rc<RefCell<Simulator>>` and manual borrow management go away.

### 2. Register-in-memory shim (immediate, low-cost)

The current RISC Zero VM backs registers at memory addresses
`0xffff_0000` (machine) and `0xffff_0080` (user). Add a compatibility
shim in `Memory::read_mem`/`Memory::write_mem` so that reads/writes to
the `0xffff_0000..0xffff_0100` range redirect to the flat register array.
This makes `load_region(Peek, USER_REGS_ADDR, ...)` work without a full
memory-model rewrite.

Affected locations:

- `src/vm/memory.rs` — `read_mem_with_privileges` and
  `write_mem_with_privileges`

### 3. `LoadOp` semantics: Peek vs Record

The upstream distinguishes:

- `LoadOp::Peek` — read-only inspection (GDB, syscall argument peeking).
  Does not affect proof state.
- `LoadOp::Record` — execution read that feeds segment accounting and
  proof witness generation.

Add an `op: LoadOp` parameter to `load_u32`/`load_region` on the trait.
GDB always uses `Peek`. Normal instruction execution uses `Record`. The
current `SessionCycleCount` callbacks only need to fire on `Record`.

### 4. Privilege-mode-aware GDB register access

Currently in `debug/readwrite.rs`, GDB reads/writes all 32 registers
flat. The upstream selects machine vs user register bank based on
`get_machine_mode()`. For now, since we run flat user mode only, this
is a no-op — but add `get_machine_mode()` to the trait so the GDB code
is ready when we add the mode split. The `read_registers` impl would
look like:

```rust
let base = if self.vm.get_machine_mode() != 0 {
    MACHINE_REGS_ADDR
} else {
    USER_REGS_ADDR
};
for i in 0..REG_MAX {
    regs.x[i] = self.vm.load_register(i);
}
regs.pc = self.vm.get_pc();
```

### 5. Separate Emulator from Context

The upstream separates instruction decode (`Emulator`) from machine
state (`Risc0Context`). Currently the `Simulator` conflates both:
it wraps `InstructionExecutor` but also owns ecall dispatch and I/O.
The trait-based approach naturally separates them:

```
Simulator implements VmContext (state: memory, registers, I/O)
rrs_lib::InstructionExecutor runs on VmContext (decode + execute)
Debugger requests go through VmContext (inspection only)
```

When we eventually swap `rrs_lib` for a cycle-exact `Executor`, we
write a new `impl VmContext` — the GDB layer is untouched.

## Files In The Full Refactor

| File | Change |
|------|--------|
| `src/vm/mod.rs` | Add `VmContext` trait, `LoadOp` enum, `MACHINE_REGS_ADDR`/`USER_REGS_ADDR` constants |
| `src/vm/simulator.rs` | `impl VmContext for Simulator` — move step/ecall/IO logic into trait methods; no structural changes to internal state |
| `src/vm/memory.rs` or `src/vm/simulator.rs` | Add register-memory shim for `0xffff_0000..0xffff_0100` range |
| `src/debug/debugger.rs` | `Simulator` -> `impl VmContext`, remove `Rc<RefCell<>>` |
| `src/debug/readwrite.rs` | `hart_state.registers` -> `vm.load_register()`/`vm.store_register()`; privilege-mode-aware bank selection |
| `src/debug/step.rs` | `simulator.borrow_mut()` -> `vm.step()` |
| `src/debug/breakpoints.rs` | `mem.watch_trigger` -> `vm.get_watch_trigger()` (or similar) |
| `src/debug/host_io.rs` | No structural changes (already reads raw ELF bytes) |
| `src/debug/monitor.rs` | `session_cycle_count` -> `vm.get_cycle_count()` |
| `src/main.rs` | Construct `Simulator`, wrap in `Debugger::<Simulator>::new()` |

## What Stays The Same In The Full Refactor

- The GDB stub integration (`gdbstub::Target` impls) — fully preserved
- `HostIo` (ELF file serving), `MonitorCmd` (cycle info), `SwBreakpoint`,
  `HwWatchpoint` — all preserved
- All ecall constants (`src/vm/mod.rs`) — no changes
- `bibc.rs` (BIGINT2 decoder) — no changes
- `syscall.rs` (POSIX-like syscall handling) — no changes
- `session_cycle.rs` (segment accounting) — no changes
- `loader.rs` (ELF loader) — no changes
- All 24 existing unit tests — should continue to pass

## Suggested Rollout Order

1. Define `VmContext` trait and `LoadOp` enum in `src/vm/mod.rs`
2. Implement `VmContext for Simulator` in `src/vm/simulator.rs`
3. Add register-memory shim in `src/vm/memory.rs`
4. Refactor `src/debug/readwrite.rs` to use trait methods
5. Refactor `src/debug/step.rs` to use `vm.step()`
6. Refactor `src/debug/debugger.rs` to hold `impl VmContext`
7. Refactor `src/debug/monitor.rs` to use `vm.get_cycle_count()`
8. Update `src/main.rs` wiring
9. Run all tests — verify no regressions

## Follow-up Implemented

The first compatibility slice has been applied without replacing the
`rrs_lib` executor:

- `src/vm/mod.rs` now defines the register-bank constants and a `VmContext`
  trait for the debugger-facing VM surface.
- `src/vm/simulator.rs` implements that trait and exposes a debug-memory shim
  for the modern RISC Zero register windows at `0xffff_0000` and
  `0xffff_0080`.
- `src/debug/readwrite.rs`, `src/debug/breakpoints.rs`, and
  `src/debug/monitor.rs` now use the VM surface for register access,
  debug-memory access, hardware watchpoint edits, and simple cycle reads.

Still future work: making `Debugger` generic over `VmContext`, adding full
`LoadOp::Peek` / `LoadOp::Record` plumbing, and swapping the backend to the
current privilege-mode-aware RISC Zero executor.

## Native Executor Follow-up

The next slice added a separate native bridge instead of forcing the custom
GDB target to switch backends all at once:

- `src/vm/native.rs` wraps raw user ELFs with
  `risc0_zkos_v1compat::V1COMPAT_ELF` and builds a current
  `risc0_zkvm::ExecutorImpl`.
- `cargo run -- --native-smoke` runs a bounded native executor smoke.
- `cargo run -- --native-gdb` delegates to RISC Zero's built-in
  `ExecutorImpl::run_with_debugger()`.
- Native execution is run on a larger worker-thread stack; the official
  executor overflowed the binary's default main-thread stack during the first
  smoke.

The native bridge proves the path is live: the packaged current
`risc0-zkvm-5.0.0-rc.1/examples/loop.bin` ELF halts with `Halted(0)`.
The checked-in historical `code` ELF is not transparently runnable under the
current v1compat kernel; it constructs the executor, then fails with an
`Illegal trap in machine mode` at `0xc0000a68`. That means the remaining
migration work is not just API plumbing: either rebuild the guest with a
current RISC Zero toolchain or preserve the standalone compatibility executor
for old 0.19-era artifacts.

## References

- Upstream gdb.rs: `risc0/circuit/rv32im/src/execute/gdb.rs` at `e459039c`
- Upstream executor.rs: `risc0/circuit/rv32im/src/execute/executor.rs`
- Upstream r0vm.rs: `risc0/circuit/rv32im/src/execute/r0vm.rs`
- Upstream platform.rs: `risc0/circuit/rv32im/src/execute/platform.rs`
- Current project notes: `RISC0_VM_UPGRADE_NOTES.md`
