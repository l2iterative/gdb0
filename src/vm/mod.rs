use gdbstub::target::ext::breakpoints::WatchKind;
use rrs_lib::MemAccessSize;

mod bibc;
pub mod loader;
pub mod memory;
pub mod native;
pub mod session_cycle;
pub mod simulator;
mod syscall;

#[allow(unused)]
pub mod reg_abi {
    pub const REG_ZERO: usize = 0; // zero constant
    pub const REG_RA: usize = 1; // return address
    pub const REG_SP: usize = 2; // stack pointer
    pub const REG_GP: usize = 3; // global pointer
    pub const REG_TP: usize = 4; // thread pointer
    pub const REG_T0: usize = 5; // temporary
    pub const REG_T1: usize = 6; // temporary
    pub const REG_T2: usize = 7; // temporary
    pub const REG_S0: usize = 8; // saved register
    pub const REG_FP: usize = 8; // frame pointer
    pub const REG_S1: usize = 9; // saved register
    pub const REG_A0: usize = 10; // fn arg / return value
    pub const REG_A1: usize = 11; // fn arg / return value
    pub const REG_A2: usize = 12; // fn arg
    pub const REG_A3: usize = 13; // fn arg
    pub const REG_A4: usize = 14; // fn arg
    pub const REG_A5: usize = 15; // fn arg
    pub const REG_A6: usize = 16; // fn arg
    pub const REG_A7: usize = 17; // fn arg
    pub const REG_S2: usize = 18; // saved register
    pub const REG_S3: usize = 19; // saved register
    pub const REG_S4: usize = 20; // saved register
    pub const REG_S5: usize = 21; // saved register
    pub const REG_S6: usize = 22; // saved register
    pub const REG_S7: usize = 23; // saved register
    pub const REG_S8: usize = 24; // saved register
    pub const REG_S9: usize = 25; // saved register
    pub const REG_S10: usize = 26; // saved register
    pub const REG_S11: usize = 27; // saved register
    pub const REG_T3: usize = 28; // temporary
    pub const REG_T4: usize = 29; // temporary
    pub const REG_T5: usize = 30; // temporary
    pub const REG_T6: usize = 31; // temporary
    pub const REG_MAX: usize = 32; // maximum number of registers
}

pub const WORD_SIZE: u32 = 4;
pub const REG_BANK_BYTES: u32 = reg_abi::REG_MAX as u32 * WORD_SIZE;
pub const MACHINE_REGS_ADDR: u32 = 0xffff_0000;
pub const USER_REGS_ADDR: u32 = 0xffff_0080;

pub mod ecall {
    pub const HALT: u32 = 0;
    pub const INPUT: u32 = 1;
    pub const SOFTWARE: u32 = 2;
    pub const SHA: u32 = 3;
    pub const BIGINT: u32 = 4;
    pub const USER: u32 = 5;
    pub const BIGINT2: u32 = 6;
    pub const POSEIDON2: u32 = 7;
}

pub mod halt {
    pub const TERMINATE: u32 = 0;
    pub const PAUSE: u32 = 1;
    pub const SPLIT: u32 = 2;
}

pub mod syscall_id {
    pub const UNKNOWN: u32 = 0;
    pub const ARGC: u32 = 1;
    pub const ARGV: u32 = 2;
    pub const CYCLE_COUNT: u32 = 3;
    pub const EXIT: u32 = 4;
    pub const FORK: u32 = 5;
    pub const GETENV: u32 = 6;
    pub const KECCAK: u32 = 7;
    pub const LOG: u32 = 8;
    pub const PANIC: u32 = 9;
    pub const PIPE: u32 = 10;
    pub const RANDOM: u32 = 11;
    pub const READ: u32 = 12;
    pub const USER: u32 = 13;
    pub const VERIFY_INTEGRITY: u32 = 14;
    pub const VERIFY_INTEGRITY2: u32 = 15;
    pub const WRITE: u32 = 16;
    pub const PROVE_ZKR: u32 = 17;
}

pub mod keccak_mode {
    pub const KECCAK_PERMUTE: u32 = 0;
    pub const KECCAK_PROVE: u32 = 1;
}

pub mod host_ecall {
    pub const TERMINATE: u32 = 0;
    pub const READ: u32 = 1;
    pub const WRITE: u32 = 2;
    pub const POSEIDON2: u32 = 3;
    pub const SHA2: u32 = 4;
    pub const BIGINT: u32 = 5;
}

pub const MAX_IO_BYTES: u32 = 1024;

pub mod poseidon2 {
    pub const PFLAG_IS_ELEM: u32 = 0x8000_0000;
    pub const PFLAG_CHECK_OUT: u32 = 0x4000_0000;
}

/// Standard IO file descriptors for use with sys_read and sys_write.
pub mod fileno {
    pub const STDIN: u32 = 0;
    pub const STDOUT: u32 = 1;
    pub const STDERR: u32 = 2;
    pub const JOURNAL: u32 = 3;
}

/// Indicates how a Segment or Session's execution has terminated
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExitCode {
    /// A user may manually pause a session so that it can be resumed at a later
    /// time, along with the user returned code.
    Paused(u32),

    /// This indicates normal termination of a program with an interior exit
    /// code returned from the guest.
    Halted(u32),

    /// HwWatchPoint
    HwWatchPoint((WatchKind, u32)),
}

pub trait VmContext {
    fn get_pc(&self) -> u32;
    fn set_pc(&mut self, pc: u32);
    fn get_machine_mode(&self) -> u32;
    fn set_machine_mode(&mut self, mode: u32);

    fn load_register(&self, idx: usize) -> Option<u32>;
    fn store_register(&mut self, idx: usize, word: u32) -> bool;

    fn read_debug_mem(&mut self, addr: u32, size: MemAccessSize) -> Option<u32>;
    fn write_debug_mem(&mut self, addr: u32, size: MemAccessSize, word: u32) -> bool;

    fn add_hw_watchpoint(&mut self, addr: u32, len: u32, kind: WatchKind) -> bool;
    fn remove_hw_watchpoint(&mut self, addr: u32, len: u32, kind: WatchKind) -> bool;

    fn step(&mut self) -> anyhow::Result<Option<ExitCode>>;
    fn get_cycle_count(&self) -> u64;
}
