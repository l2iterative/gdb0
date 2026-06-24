use crate::vm;
use crate::vm::reg_abi::{REG_A0, REG_A1, REG_A3, REG_A4, REG_A5, REG_A6};
use crate::vm::simulator::Simulator;
use crate::vm::ExitCode;
use anyhow::{anyhow, bail, Result};
use rrs_lib::{MemAccessSize, Memory};
use std::str::from_utf8;

const WORD_SIZE: usize = core::mem::size_of::<u32>();
const KECCAK_STATE_BYTES: usize = 200;
const KECCAK_STATE_WORDS: usize = KECCAK_STATE_BYTES / WORD_SIZE;
const CURRENT_RET_WORDS: usize = 2;
const MAX_IO_BYTES: u32 = vm::MAX_IO_BYTES;
const MAX_PANIC_MSG_SIZE: u32 = 1 << 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SyscallKind {
    Argc,
    Argv,
    CycleCount,
    Exit,
    Fork,
    Getenv,
    Keccak,
    Log,
    Panic,
    Pipe,
    Random,
    Read,
    ReadAvail,
    User,
    VerifyIntegrity,
    VerifyIntegrity2,
    ProveZkr,
    Write,
}

fn syscall_suffix(syscall_name: &str) -> &str {
    syscall_name.rsplit("::").next().unwrap_or(syscall_name)
}

fn kind_from_name(syscall_name: &str) -> Option<SyscallKind> {
    match syscall_suffix(syscall_name) {
        "SYS_ARGC" => Some(SyscallKind::Argc),
        "SYS_ARGV" | "SYS_ARGS" => Some(SyscallKind::Argv),
        "SYS_CYCLE_COUNT" => Some(SyscallKind::CycleCount),
        "SYS_EXIT" => Some(SyscallKind::Exit),
        "SYS_FORK" => Some(SyscallKind::Fork),
        "SYS_GETENV" => Some(SyscallKind::Getenv),
        "SYS_KECCAK" | "SYS_PROVE_KECCAK" => Some(SyscallKind::Keccak),
        "SYS_LOG" => Some(SyscallKind::Log),
        "SYS_PANIC" => Some(SyscallKind::Panic),
        "SYS_PIPE" => Some(SyscallKind::Pipe),
        "SYS_RANDOM" => Some(SyscallKind::Random),
        "SYS_READ" => Some(SyscallKind::Read),
        "SYS_READ_AVAIL" => Some(SyscallKind::ReadAvail),
        "SYS_VERIFY" | "SYS_VERIFY_INTEGRITY" => Some(SyscallKind::VerifyIntegrity),
        "SYS_VERIFY_INTEGRITY2" => Some(SyscallKind::VerifyIntegrity2),
        "SYS_PROVE_ZKR" => Some(SyscallKind::ProveZkr),
        "SYS_WRITE" => Some(SyscallKind::Write),
        _ => None,
    }
}

fn kind_from_id(syscall_id: u32) -> Option<SyscallKind> {
    match syscall_id {
        vm::syscall_id::ARGC => Some(SyscallKind::Argc),
        vm::syscall_id::ARGV => Some(SyscallKind::Argv),
        vm::syscall_id::CYCLE_COUNT => Some(SyscallKind::CycleCount),
        vm::syscall_id::EXIT => Some(SyscallKind::Exit),
        vm::syscall_id::FORK => Some(SyscallKind::Fork),
        vm::syscall_id::GETENV => Some(SyscallKind::Getenv),
        vm::syscall_id::KECCAK => Some(SyscallKind::Keccak),
        vm::syscall_id::LOG => Some(SyscallKind::Log),
        vm::syscall_id::PANIC => Some(SyscallKind::Panic),
        vm::syscall_id::PIPE => Some(SyscallKind::Pipe),
        vm::syscall_id::RANDOM => Some(SyscallKind::Random),
        vm::syscall_id::READ => Some(SyscallKind::Read),
        vm::syscall_id::USER => Some(SyscallKind::User),
        vm::syscall_id::VERIFY_INTEGRITY => Some(SyscallKind::VerifyIntegrity),
        vm::syscall_id::VERIFY_INTEGRITY2 => Some(SyscallKind::VerifyIntegrity2),
        vm::syscall_id::PROVE_ZKR => Some(SyscallKind::ProveZkr),
        vm::syscall_id::WRITE => Some(SyscallKind::Write),
        _ => None,
    }
}

pub(crate) fn classify_syscall(syscall_name: &str, syscall_id: u32) -> Result<(SyscallKind, bool)> {
    let name_kind = kind_from_name(syscall_name);
    let id_kind = kind_from_id(syscall_id);

    match (name_kind, id_kind) {
        (Some(name_kind), Some(id_kind)) if name_kind == id_kind => Ok((name_kind, true)),
        (Some(name_kind), _) => Ok((name_kind, false)),
        (None, Some(id_kind)) => Ok((id_kind, true)),
        (None, None) => bail!("Unknown syscall: {syscall_name:?}"),
    }
}

fn read_guest_bytes(vm: &mut Simulator, ptr: u32, len: u32) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(len as usize);
    for i in 0..len {
        bytes.push(
            (*vm.mem)
                .borrow_mut()
                .read_mem(ptr + i, MemAccessSize::Byte)
                .ok_or_else(|| anyhow!("guest memory at 0x{:08x} cannot be read", ptr + i))?
                as u8,
        );
    }
    Ok(bytes)
}

fn copy_bytes_to_guest_buffer(to_guest: &mut [u32], bytes: &[u8]) -> usize {
    let nbytes = core::cmp::min(to_guest.len() * WORD_SIZE, bytes.len());
    let to_guest_u8s: &mut [u8] = bytemuck::cast_slice_mut(to_guest);
    to_guest_u8s[0..nbytes].clone_from_slice(&bytes[0..nbytes]);
    nbytes
}

fn read_final_word(nread: usize, buf: &[u8]) -> u32 {
    let tail = nread % WORD_SIZE;
    if tail == 0 || nread == 0 {
        return 0;
    }

    let start = nread - tail;
    let mut final_word_bytes = [0u8; WORD_SIZE];
    final_word_bytes[..tail].copy_from_slice(&buf[start..nread]);
    u32::from_le_bytes(final_word_bytes)
}

fn set_return(vm: &mut Simulator, a0: u32, a1: u32) {
    vm.hart_state.registers[REG_A0] = a0;
    vm.hart_state.registers[REG_A1] = a1;
}

fn set_return_buffer(to_guest: &mut [u32], a0: u32, a1: u32) -> Result<()> {
    if to_guest.len() != CURRENT_RET_WORDS {
        bail!("current syscall return buffer must be two words");
    }

    to_guest[0] = a0;
    to_guest[1] = a1;
    Ok(())
}

pub fn handle_syscall(
    syscall_name: &str,
    syscall_id: u32,
    to_guest: &mut [u32],
    vm: &mut Simulator,
) -> Result<Option<ExitCode>> {
    let (syscall_kind, current_abi) = classify_syscall(syscall_name, syscall_id)?;

    match syscall_kind {
        SyscallKind::Random => {
            let mut rand_buf = vec![0u8; to_guest.len() * WORD_SIZE];
            getrandom::getrandom(rand_buf.as_mut_slice())?;
            bytemuck::cast_slice_mut(to_guest).clone_from_slice(rand_buf.as_slice());
            set_return(vm, rand_buf.len() as u32, 0);
        }
        SyscallKind::CycleCount => {
            let cycle = vm.session_cycle_count.borrow().get_session_cycle() as u64;
            if current_abi {
                let hi = (cycle >> 32) as u32;
                let lo = cycle as u32;
                if !to_guest.is_empty() {
                    set_return_buffer(to_guest, hi, lo)?;
                    set_return(vm, (CURRENT_RET_WORDS * WORD_SIZE) as u32, 0);
                } else {
                    set_return(vm, hi, lo);
                }
            } else {
                set_return(vm, cycle as u32, 0);
            }
        }
        SyscallKind::Panic => {
            if !to_guest.is_empty() {
                bail!("invalid sys_panic call");
            }
            let buf_ptr = vm.hart_state.registers[REG_A3];
            let buf_len = u32::min(vm.hart_state.registers[REG_A4], MAX_PANIC_MSG_SIZE);
            let from_guest = read_guest_bytes(vm, buf_ptr, buf_len)?;
            let msg = from_utf8(&from_guest)?;
            bail!("Guest panicked: {msg}");
        }
        SyscallKind::Getenv => {
            let buf_ptr = vm.hart_state.registers[REG_A3];
            let buf_len = vm.hart_state.registers[REG_A4];
            let from_guest = read_guest_bytes(vm, buf_ptr, u32::min(buf_len, MAX_IO_BYTES + 1))?;
            let msg = from_utf8(&from_guest)?;
            if msg.len() > MAX_IO_BYTES as usize {
                bail!("sys_getenv failure: env var name is too large");
            }

            match vm.env.get(msg) {
                None if to_guest.is_empty() => {
                    set_return(vm, u32::MAX, 0);
                }
                None => set_return(vm, u32::MAX, 0),
                Some(val) => {
                    if val.len() >= MAX_IO_BYTES as usize {
                        bail!("sys_getenv failure: value is too large");
                    }
                    copy_bytes_to_guest_buffer(to_guest, val.as_bytes());
                    set_return(vm, val.len() as u32, 0);
                }
            }
        }
        SyscallKind::Read => {
            let fd = vm.hart_state.registers[REG_A3];
            let requested_bytes = vm.hart_state.registers[REG_A4] as usize;

            let to_guest_u8 = bytemuck::cast_slice_mut(to_guest);
            let read_limit = core::cmp::min(to_guest_u8.len(), requested_bytes);

            let mut read_all = |mut buf: &mut [u8]| -> Result<usize> {
                let mut tot_nread = 0;
                while !buf.is_empty() {
                    let nread = vm.read_from_fd(fd, buf)?;
                    if nread == 0 {
                        break;
                    }
                    tot_nread += nread;
                    (_, buf) = buf.split_at_mut(nread);
                }
                Ok(tot_nread)
            };

            let nread = read_all(&mut to_guest_u8[..read_limit])?;
            let final_word = read_final_word(nread, to_guest_u8);
            set_return(vm, nread as u32, final_word);
        }
        SyscallKind::ReadAvail => {
            let fd = vm.hart_state.registers[REG_A3];
            set_return(vm, vm.read_fd_available(fd)?, 0);
        }
        SyscallKind::Write => {
            if !to_guest.is_empty() {
                bail!("invalid sys_write call");
            }
            let fd = vm.hart_state.registers[REG_A3];
            let buf_ptr = vm.hart_state.registers[REG_A4];
            let buf_len = vm.hart_state.registers[REG_A5];
            let from_guest = read_guest_bytes(vm, buf_ptr, buf_len)?;
            vm.write_to_fd(fd, from_guest.as_slice())?;
            set_return(vm, 0, 0);
        }
        SyscallKind::Log => {
            if !to_guest.is_empty() {
                bail!("invalid sys_log call");
            }
            let buf_ptr = vm.hart_state.registers[REG_A3];
            let buf_len = vm.hart_state.registers[REG_A4];
            let from_guest = read_guest_bytes(vm, buf_ptr, buf_len)?;
            if current_abi {
                let msg = format!(
                    "R0VM[{}] ",
                    vm.session_cycle_count.borrow().get_session_cycle()
                );
                vm.write_to_fd(vm::fileno::STDOUT, msg.as_bytes())?;
                vm.write_to_fd(vm::fileno::STDOUT, from_guest.as_slice())?;
                vm.write_to_fd(vm::fileno::STDOUT, b"\n")?;
            } else {
                vm.write_to_fd(vm::fileno::STDOUT, from_guest.as_slice())?;
            }
            set_return(vm, 0, 0);
        }
        SyscallKind::VerifyIntegrity | SyscallKind::VerifyIntegrity2 | SyscallKind::ProveZkr => {
            if !to_guest.is_empty() {
                bail!("invalid verify syscall call");
            }
            set_return(vm, 0, 0);
        }
        SyscallKind::Argc => {
            if current_abi && !to_guest.is_empty() {
                set_return_buffer(to_guest, vm.args.len() as u32, 0)?;
                set_return(vm, (CURRENT_RET_WORDS * WORD_SIZE) as u32, 0);
            } else {
                set_return(vm, vm.args.len() as u32, 0);
            }
        }
        SyscallKind::Argv => {
            let arg_index = vm.hart_state.registers[REG_A3];
            let arg_val = vm.args.get(arg_index as usize).ok_or_else(|| {
                anyhow!(
                    "guest requested index {arg_index} from argv of len {}",
                    vm.args.len()
                )
            })?;
            if arg_val.len() >= MAX_IO_BYTES as usize {
                bail!("sys_argv failure: argv is too large");
            }

            copy_bytes_to_guest_buffer(to_guest, arg_val.as_bytes());
            set_return(vm, arg_val.len() as u32, 0);
        }
        SyscallKind::Pipe => {
            if to_guest.len() != 2 {
                bail!("invalid sys_pipe call");
            }
            let (read_fd, write_fd) = vm.allocate_pipe()?;
            to_guest[0] = read_fd;
            to_guest[1] = write_fd;
            set_return(vm, 0, 0);
        }
        SyscallKind::Fork => {
            if !to_guest.is_empty() {
                bail!("invalid sys_fork call");
            }
            set_return(vm, u32::MAX, 0);
        }
        SyscallKind::Exit => {
            if !to_guest.is_empty() {
                bail!("invalid sys_exit call");
            }
            return Ok(Some(ExitCode::Halted(0)));
        }
        SyscallKind::Keccak => {
            handle_keccak(to_guest, vm)?;
        }
        SyscallKind::User => {
            bail!("custom USER syscalls are not implemented in this standalone VM");
        }
    }

    Ok(None)
}

fn handle_keccak(to_guest: &mut [u32], vm: &mut Simulator) -> Result<()> {
    if vm.hart_state.registers[REG_A6] != 0 && to_guest.len() == CURRENT_RET_WORDS {
        set_return_buffer(to_guest, 0, 0)?;
        set_return(vm, (CURRENT_RET_WORDS * WORD_SIZE) as u32, 0);
        return Ok(());
    }

    let mode = vm.hart_state.registers[REG_A3];

    match mode {
        vm::keccak_mode::KECCAK_PERMUTE => {
            if to_guest.len() != KECCAK_STATE_WORDS {
                bail!("invalid sys_keccak permutation output size");
            }

            let in_state_ptr = vm.hart_state.registers[REG_A4];
            let in_state = read_guest_bytes(vm, in_state_ptr, KECCAK_STATE_BYTES as u32)?;
            let mut state = [0u64; KECCAK_STATE_BYTES / 8];
            for (word, bytes) in state.iter_mut().zip(in_state.chunks_exact(8)) {
                *word = u64::from_le_bytes(bytes.try_into().unwrap());
            }

            keccak::f1600(&mut state);

            let state_words: &[u32] = bytemuck::cast_slice(&state);
            to_guest.clone_from_slice(state_words);
            set_return(vm, 0, 0);
        }
        vm::keccak_mode::KECCAK_PROVE => {
            if !to_guest.is_empty() {
                bail!("invalid sys_keccak prove call");
            }
            set_return(vm, 0, 0);
        }
        _ => bail!("sys_keccak: invalid mode: {mode}"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::memory::Memory as GuestMemory;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    fn new_simulator() -> Simulator {
        let mem = Rc::new(RefCell::new(GuestMemory::default()));
        Simulator::new(mem, 0x10000, &HashMap::new())
    }

    fn write_guest_bytes(vm: &mut Simulator, ptr: u32, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            assert!(vm.mem.borrow_mut().write_mem(
                ptr + i as u32,
                MemAccessSize::Byte,
                *byte as u32,
            ));
        }
    }

    #[test]
    fn classifies_current_syscall_id_when_name_matches() {
        let (kind, current_abi) =
            classify_syscall("risc0_zkvm_platform::syscall::nr::SYS_CYCLE_COUNT", 3).unwrap();
        assert_eq!(kind, SyscallKind::CycleCount);
        assert!(current_abi);
    }

    #[test]
    fn prefers_legacy_name_when_stale_id_disagrees() {
        let (kind, current_abi) =
            classify_syscall("risc0_zkvm_platform::syscall::nr::SYS_READ_AVAIL", 12).unwrap();
        assert_eq!(kind, SyscallKind::ReadAvail);
        assert!(!current_abi);
    }

    #[test]
    fn supports_argv_and_legacy_args_names() {
        assert_eq!(
            classify_syscall("risc0_zkvm_platform::syscall::nr::SYS_ARGV", 2).unwrap(),
            (SyscallKind::Argv, true)
        );
        assert_eq!(
            classify_syscall("risc0_zkvm_platform::syscall::nr::SYS_ARGS", 0).unwrap(),
            (SyscallKind::Argv, false)
        );
    }

    #[test]
    fn maps_prove_zkr_name_and_id_to_verify_path() {
        assert_eq!(
            classify_syscall("risc0_zkvm_platform::syscall::nr::SYS_PROVE_ZKR", 17).unwrap(),
            (SyscallKind::ProveZkr, true)
        );
    }

    #[test]
    fn maps_prove_zkr_id_to_verify_path() {
        let mut vm = new_simulator();
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_UNKNOWN",
            vm::syscall_id::PROVE_ZKR,
            &mut [],
            &mut vm,
        )
        .unwrap();
        assert_eq!(vm.hart_state.registers[REG_A0], 0);
        assert_eq!(vm.hart_state.registers[REG_A1], 0);
    }

    #[test]
    fn cycle_count_uses_current_hi_lo_registers_only_with_matching_id() {
        let mut vm = new_simulator();
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_CYCLE_COUNT",
            vm::syscall_id::CYCLE_COUNT,
            &mut [],
            &mut vm,
        )
        .unwrap();
        let cycle = vm.session_cycle_count.borrow().get_session_cycle() as u64;
        assert_eq!(vm.hart_state.registers[REG_A0], (cycle >> 32) as u32);
        assert_eq!(vm.hart_state.registers[REG_A1], cycle as u32);

        let mut legacy_vm = new_simulator();
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_CYCLE_COUNT",
            vm::syscall_id::UNKNOWN,
            &mut [],
            &mut legacy_vm,
        )
        .unwrap();
        assert_eq!(legacy_vm.hart_state.registers[REG_A0], cycle as u32);
        assert_eq!(legacy_vm.hart_state.registers[REG_A1], 0);
    }

    #[test]
    fn cycle_count_can_fill_current_two_word_return_buffer() {
        let mut vm = new_simulator();
        let mut to_guest = [0u32; 2];
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_CYCLE_COUNT",
            vm::syscall_id::CYCLE_COUNT,
            &mut to_guest,
            &mut vm,
        )
        .unwrap();

        let cycle = vm.session_cycle_count.borrow().get_session_cycle() as u64;
        assert_eq!(to_guest, [(cycle >> 32) as u32, cycle as u32]);
        assert_eq!(vm.hart_state.registers[REG_A0], 8);
    }

    #[test]
    fn argc_and_argv_support_current_return_buffer_shape() {
        let mut vm = new_simulator();
        vm.args(&["alpha".to_string(), "beta".to_string()]);

        let mut argc = [0u32; 2];
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_ARGC",
            vm::syscall_id::ARGC,
            &mut argc,
            &mut vm,
        )
        .unwrap();
        assert_eq!(argc, [2, 0]);
        assert_eq!(vm.hart_state.registers[REG_A0], 8);

        vm.hart_state.registers[REG_A3] = 1;
        let mut argv_len = [0u32; 2];
        let expected = u32::from_le_bytes(*b"beta");
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_ARGV",
            vm::syscall_id::ARGV,
            &mut argv_len,
            &mut vm,
        )
        .unwrap();
        let argv_len_bytes: [u8; 4] = bytemuck::cast_slice(&argv_len)[0..4]
            .try_into()
            .expect("argv buffer should contain at least 4 bytes");
        assert_eq!(u32::from_le_bytes(argv_len_bytes), expected);
        assert_eq!(vm.hart_state.registers[REG_A0], 4);
        assert_eq!(vm.hart_state.registers[REG_A1], 0);
    }

    #[test]
    fn argc_and_argv_support_current_length_query_shape() {
        let mut vm = new_simulator();
        vm.args(&["alpha".to_string(), "beta".to_string()]);

        let mut argv_len = [0u32; 0];
        vm.hart_state.registers[REG_A3] = 1;
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_ARGV",
            vm::syscall_id::ARGV,
            &mut argv_len,
            &mut vm,
        )
        .unwrap();
        assert_eq!(vm.hart_state.registers[REG_A0], 4);
        assert_eq!(vm.hart_state.registers[REG_A1], 0);
    }

    #[test]
    fn getenv_supports_current_length_query_shape() {
        let mut vm = new_simulator();
        vm.env.insert("RUST_BACKTRACE".into(), "1".into());
        let name_ptr = 0x11000;
        write_guest_bytes(&mut vm, name_ptr, b"RUST_BACKTRACE");
        vm.hart_state.registers[REG_A3] = name_ptr;
        vm.hart_state.registers[REG_A4] = "RUST_BACKTRACE".len() as u32;

        let mut getenv_len = [0u32; 0];
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_GETENV",
            vm::syscall_id::GETENV,
            &mut getenv_len,
            &mut vm,
        )
        .unwrap();

        assert_eq!(vm.hart_state.registers[REG_A0], 1);
        assert_eq!(vm.hart_state.registers[REG_A1], 0);
    }

    #[test]
    fn keccak_check_full_uses_current_two_word_return_buffer() {
        let mut vm = new_simulator();
        vm.hart_state.registers[REG_A6] = 1;

        let mut is_full = [u32::MAX; 2];
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_KECCAK",
            vm::syscall_id::KECCAK,
            &mut is_full,
            &mut vm,
        )
        .unwrap();

        assert_eq!(is_full, [0, 0]);
        assert_eq!(vm.hart_state.registers[REG_A0], 8);
    }

    #[test]
    fn read_returns_short_read_on_eof() {
        let mut vm = new_simulator();
        vm.write(vm::fileno::STDIN, b"hi").unwrap();
        vm.hart_state.registers[REG_A3] = vm::fileno::STDIN;
        vm.hart_state.registers[REG_A4] = 8;

        let mut to_guest = [0u32; 1];
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_READ",
            vm::syscall_id::READ,
            &mut to_guest,
            &mut vm,
        )
        .unwrap();

        assert_eq!(vm.hart_state.registers[REG_A0], 2);
        assert_eq!(
            vm.hart_state.registers[REG_A1],
            u32::from_le_bytes(*b"hi\0\0")
        );
        assert_eq!(to_guest[0] & 0xffff, u16::from_le_bytes(*b"hi") as u32);
    }

    #[test]
    fn read_returns_trailing_word_when_request_is_unaligned() {
        let mut vm = new_simulator();
        vm.write(vm::fileno::STDIN, b"abcde").unwrap();
        vm.hart_state.registers[REG_A3] = vm::fileno::STDIN;
        vm.hart_state.registers[REG_A4] = 5;

        let mut to_guest = [0u32; 2];
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_READ",
            vm::syscall_id::READ,
            &mut to_guest,
            &mut vm,
        )
        .unwrap();

        assert_eq!(vm.hart_state.registers[REG_A0], 5);
        assert_eq!(
            vm.hart_state.registers[REG_A1],
            u32::from_le_bytes([b'e', 0, 0, 0])
        );
    }

    #[test]
    fn read_writes_only_requested_bytes_and_preserves_tail() {
        let mut vm = new_simulator();
        vm.write(vm::fileno::STDIN, b"abcdef").unwrap();
        vm.hart_state.registers[REG_A3] = vm::fileno::STDIN;
        vm.hart_state.registers[REG_A4] = 5;

        let mut to_guest = [0u32; 2];
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_READ",
            vm::syscall_id::READ,
            &mut to_guest,
            &mut vm,
        )
        .unwrap();

        assert_eq!(vm.hart_state.registers[REG_A0], 5);
        assert_eq!(
            vm.hart_state.registers[REG_A1],
            u32::from_le_bytes(*b"e\0\0\0")
        );

        let to_guest = bytemuck::cast_slice::<u32, u8>(&to_guest);
        assert_eq!(&to_guest[..5], b"abcde");
        assert_eq!(&to_guest[5..], &[0, 0, 0]);
    }

    #[test]
    fn pipe_fds_can_write_then_read() {
        let mut vm = new_simulator();
        let mut pipe_fds = [0u32; 2];
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_PIPE",
            vm::syscall_id::PIPE,
            &mut pipe_fds,
            &mut vm,
        )
        .unwrap();

        let msg_ptr = 0x11000;
        write_guest_bytes(&mut vm, msg_ptr, b"ok!!");
        vm.hart_state.registers[REG_A3] = pipe_fds[1];
        vm.hart_state.registers[REG_A4] = msg_ptr;
        vm.hart_state.registers[REG_A5] = 4;
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_WRITE",
            vm::syscall_id::WRITE,
            &mut [],
            &mut vm,
        )
        .unwrap();

        vm.hart_state.registers[REG_A3] = pipe_fds[0];
        vm.hart_state.registers[REG_A4] = 4;
        let mut to_guest = [0u32; 1];
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_READ",
            vm::syscall_id::READ,
            &mut to_guest,
            &mut vm,
        )
        .unwrap();

        assert_eq!(vm.hart_state.registers[REG_A0], 4);
        assert_eq!(to_guest[0], u32::from_le_bytes(*b"ok!!"));
    }

    #[test]
    fn keccak_permutation_matches_current_platform_syscall_shape() {
        let mut vm = new_simulator();
        let state_ptr = 0x12000;
        write_guest_bytes(&mut vm, state_ptr, &[0; KECCAK_STATE_BYTES]);
        vm.hart_state.registers[REG_A3] = vm::keccak_mode::KECCAK_PERMUTE;
        vm.hart_state.registers[REG_A4] = state_ptr;

        let mut to_guest = [0u32; KECCAK_STATE_WORDS];
        handle_syscall(
            "risc0_zkvm_platform::syscall::nr::SYS_KECCAK",
            vm::syscall_id::KECCAK,
            &mut to_guest,
            &mut vm,
        )
        .unwrap();

        let lanes: &[u64] = bytemuck::cast_slice(&to_guest);
        assert_eq!(lanes[0], 0xF1258F7940E1DDE7);
        assert_eq!(lanes[24], 0xEAF1FF7B5CECA249);
        assert_eq!(vm.hart_state.registers[REG_A0], 0);
    }
}
