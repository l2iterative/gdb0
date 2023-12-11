use crate::vm;
use crate::vm::reg_abi::{REG_A0, REG_A1, REG_A3, REG_A4, REG_A5};
use crate::vm::simulator::Simulator;
use crate::vm::ExitCode;
use anyhow::{anyhow, bail, Result};
use rrs_lib::{MemAccessSize, Memory};
use std::borrow::BorrowMut;
use std::io::Read;
use std::str::from_utf8;

pub fn handle_syscall(
    syscall_name: &String,
    to_guest: &mut [u32],
    vm: &mut Simulator,
) -> Result<Option<ExitCode>> {
    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_RANDOM" {
        let mut rand_buf = vec![0u8; to_guest.len() * 4];
        getrandom::getrandom(rand_buf.as_mut_slice())?;
        bytemuck::cast_slice_mut(to_guest).clone_from_slice(rand_buf.as_slice());
        vm.hart_state.registers[REG_A0] = 0;
        vm.hart_state.registers[REG_A1] = 0;

        return Ok(None);
    }

    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_CYCLE_COUNT" {
        vm.hart_state.registers[REG_A0] =
            vm.session_cycle_count.borrow().get_session_cycle() as u32;
        vm.hart_state.registers[REG_A1] = 0;

        return Ok(None);
    }

    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_PANIC" {
        let buf_ptr = vm.hart_state.registers[REG_A3];
        let buf_len = vm.hart_state.registers[REG_A4];
        let mut from_guest = Vec::<u8>::new();

        for i in 0..buf_len {
            from_guest.push(
                (*vm.mem)
                    .borrow_mut()
                    .read_mem(buf_ptr + i, MemAccessSize::Byte)
                    .ok_or_else(|| {
                        anyhow::format_err!("message of a PANIC software syscall cannot be read")
                    })? as u8,
            );
        }
        let msg = from_utf8(&from_guest)?;

        bail!("Guest panicked: {msg}");
    }

    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_GETENV" {
        let buf_ptr = vm.hart_state.registers[REG_A3];
        let buf_len = vm.hart_state.registers[REG_A4];

        let mut from_guest = Vec::<u8>::new();
        for i in 0..buf_len {
            from_guest.push(
                (*vm.mem)
                    .borrow_mut()
                    .read_mem(buf_ptr + i, MemAccessSize::Byte)
                    .ok_or_else(|| {
                        anyhow::format_err!(
                            "environment variable name of a GETENV software syscall cannot be read"
                        )
                    })? as u8,
            );
        }

        let msg = from_utf8(&from_guest)?;

        return match vm.env.get(msg) {
            None => {
                vm.hart_state.registers[REG_A0] = u32::MAX;
                vm.hart_state.registers[REG_A1] = 0;

                Ok(None)
            }
            Some(val) => {
                let nbytes = core::cmp::min(to_guest.len() * 4, val.as_bytes().len());
                let to_guest_u8s: &mut [u8] = bytemuck::cast_slice_mut(to_guest);
                to_guest_u8s[0..nbytes].clone_from_slice(&val.as_bytes()[0..nbytes]);

                vm.hart_state.registers[REG_A0] = val.as_bytes().len() as u32;
                vm.hart_state.registers[REG_A1] = 0;

                Ok(None)
            }
        };
    }

    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_READ" {
        let fd = vm.hart_state.registers[REG_A3];
        let nbytes = vm.hart_state.registers[REG_A4] as usize;

        assert!(
            nbytes >= to_guest.len() * 4,
            "Word-aligned read buffer must be fully filled"
        );

        if fd != vm::fileno::STDIN {
            bail!("Bad read file descriptor {fd}");
        }

        let mut read_all = |mut buf: &mut [u8]| -> Result<usize> {
            let mut tot_nread = 0;
            while !buf.is_empty() {
                let nread = vm.stdin.borrow_mut().read(buf)?;
                if nread == 0 {
                    break;
                }
                tot_nread += nread;
                (_, buf) = buf.split_at_mut(nread);
            }
            Ok(tot_nread)
        };

        let to_guest_u8 = bytemuck::cast_slice_mut(to_guest);
        let nread_main = read_all(to_guest_u8)?;
        assert_eq!(
            nread_main,
            to_guest_u8.len(),
            "Guest requested more data than was available"
        );

        let unaligned_end = nbytes - nread_main;
        assert!(unaligned_end <= 4, "{unaligned_end} must be <= 4");

        // Fill unaligned word out.
        let mut to_guest_end: [u8; 4] = [0; 4];
        let nread_end = read_all(&mut to_guest_end[0..unaligned_end])?;

        vm.hart_state.registers[REG_A0] = (nread_main + nread_end) as u32;
        vm.hart_state.registers[REG_A1] = u32::from_le_bytes(to_guest_end);

        return Ok(None);
    }

    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_READ_AVAIL" {
        let fd = vm.hart_state.registers[REG_A3];

        if fd != vm::fileno::STDIN {
            bail!("Bad read file descriptor {fd}");
        }

        let navail = (vm.stdin.get_ref().len() as u64 - vm.stdin.position()) as u32;

        vm.hart_state.registers[REG_A0] = navail;
        vm.hart_state.registers[REG_A1] = 0;

        return Ok(None);
    }

    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_WRITE" {
        let fd = vm.hart_state.registers[REG_A3];
        let buf_ptr = vm.hart_state.registers[REG_A4];
        let buf_len = vm.hart_state.registers[REG_A5];

        let mut from_guest_bytes = Vec::<u8>::new();
        for i in 0..buf_len {
            from_guest_bytes.push(
                (*vm.mem)
                    .borrow_mut()
                    .read_mem(buf_ptr + i, MemAccessSize::Byte)
                    .ok_or_else(|| {
                        anyhow::format_err!("data of a WRITE software syscall cannot be read")
                    })? as u8,
            );
        }

        let fd = vm.get_write_fd(fd)?;
        fd.get_mut().extend_from_slice(from_guest_bytes.as_slice());

        vm.hart_state.registers[REG_A0] = 0;
        vm.hart_state.registers[REG_A1] = 0;

        return Ok(None);
    }

    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_LOG" {
        let buf_ptr = vm.hart_state.registers[REG_A3];
        let buf_len = vm.hart_state.registers[REG_A4];

        let mut from_guest_bytes = Vec::<u8>::new();
        for i in 0..buf_len {
            from_guest_bytes.push(
                (*vm.mem)
                    .borrow_mut()
                    .read_mem(buf_ptr + i, MemAccessSize::Byte)
                    .ok_or_else(|| {
                        anyhow::format_err!("data of a LOG software syscall cannot be read")
                    })? as u8,
            );
        }

        vm.stdout
            .get_mut()
            .extend_from_slice(from_guest_bytes.as_slice());

        vm.hart_state.registers[REG_A0] = 0;
        vm.hart_state.registers[REG_A1] = 0;

        return Ok(None);
    }

    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_VERIFY"
        || syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_VERIFY_INTEGRITY"
    {
        vm.hart_state.registers[REG_A0] = 0;
        vm.hart_state.registers[REG_A1] = 0;

        return Ok(None);
    }

    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_ARGC" {
        vm.hart_state.registers[REG_A0] = vm.args.len() as u32;
        vm.hart_state.registers[REG_A1] = 0;

        return Ok(None);
    }

    if syscall_name == "risc0_zkvm_platform::syscall::nr::SYS_ARGS" {
        let arg_index = vm.hart_state.registers[REG_A3];
        let arg_val = vm.args.get(arg_index as usize).ok_or_else(|| {
            anyhow!(
                "guest requested index {arg_index} from argv of len {}",
                vm.args.len()
            )
        })?;

        let nbytes = core::cmp::min(to_guest.len() * 4, arg_val.as_bytes().len());
        let to_guest_u8s: &mut [u8] = bytemuck::cast_slice_mut(to_guest);
        to_guest_u8s[0..nbytes].clone_from_slice(&arg_val.as_bytes()[0..nbytes]);

        vm.hart_state.registers[REG_A0] = arg_val.as_bytes().len() as u32;
        vm.hart_state.registers[REG_A1] = 0;

        return Ok(None);
    }

    Ok(None)
}
