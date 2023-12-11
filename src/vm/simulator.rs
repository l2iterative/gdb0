use crate::vm;
use crate::vm::memory::{GUEST_MAX_MEM, GUEST_MIN_MEM};
use crate::vm::session_cycle::{get_opcode_cycle, SessionCycleCount};
use crate::vm::ExitCode;
use anyhow::{anyhow, bail, Result};
use crypto_bigint::{CheckedMul, Encoding, NonZero, U256, U512};
use rrs_lib::instruction_executor::InstructionExecutor;
use rrs_lib::{HartState, MemAccessSize, Memory};
use sha2::digest::generic_array::GenericArray;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::ops::DerefMut;
use std::rc::Rc;

pub struct Simulator {
    pub mem: Rc<RefCell<vm::memory::Memory>>,
    pub hart_state: HartState,
    pub env: HashMap<String, String>,
    pub stdin: Cursor<Vec<u8>>,
    pub stdout: Cursor<Vec<u8>>,
    pub stderr: Cursor<Vec<u8>>,
    pub journal: Cursor<Vec<u8>>,
    pub args: Vec<String>,
    pub session_cycle_count: Rc<RefCell<SessionCycleCount>>,
}

impl Simulator {
    pub fn new(
        mem: Rc<RefCell<vm::memory::Memory>>,
        entry: u32,
        env: &HashMap<String, String>,
    ) -> Self {
        let mut hart_state = HartState::new();
        hart_state.pc = entry;

        let session_cycle_count = Rc::new(RefCell::new(SessionCycleCount::default()));
        mem.borrow_mut()
            .with_session_cycle_callback(session_cycle_count.clone());

        Self {
            mem,
            hart_state,
            env: env.clone(),
            stdin: Cursor::default(),
            stdout: Cursor::default(),
            stderr: Cursor::default(),
            journal: Cursor::default(),
            args: Vec::new(),
            session_cycle_count,
        }
    }

    pub fn write(&mut self, read_fd: u32, data: &[u8]) -> Result<()> {
        if read_fd == vm::fileno::STDIN {
            self.stdin.get_mut().extend_from_slice(data);
            return Ok(());
        } else {
            bail!("cannot write to an unsupported input channel.");
        }
    }

    pub(crate) fn get_write_fd(&mut self, write_fd: u32) -> Result<&mut Cursor<Vec<u8>>> {
        if write_fd == vm::fileno::STDOUT {
            return Ok(&mut self.stdout);
        } else if write_fd == vm::fileno::STDERR {
            return Ok(&mut self.stderr);
        } else if write_fd == vm::fileno::JOURNAL {
            return Ok(&mut self.journal);
        } else {
            bail!("cannot read an unsupported output channel.")
        }
    }

    pub fn read(&mut self, write_fd: u32, len: usize, dst: &mut [u8]) -> Result<()> {
        let buf = self.get_write_fd(write_fd)?;

        if buf.get_ref().len() as u64 - buf.position() < len as u64 {
            bail!("not enough data in the output channel.");
        }

        buf.read_exact(&mut dst[0..len]).map_err(|err| {
            anyhow!("cannot write to the buffer for reading the output channel. {err}")
        })?;
        Ok(())
    }

    pub fn read_to_end(&mut self, write_fd: u32, dst: &mut Vec<u8>) -> Result<()> {
        let buf = self.get_write_fd(write_fd)?;
        buf.read_to_end(dst).map_err(|err| {
            anyhow!("cannot write to the buffer for reading the output channel. {err}")
        })?;
        Ok(())
    }

    pub fn args(&mut self, args: &[String]) {
        self.args.extend_from_slice(args);
    }

    pub fn step(&mut self) -> Result<Option<ExitCode>> {
        let insn = self
            .mem
            .borrow_mut()
            .read_mem(self.hart_state.pc, MemAccessSize::Word)
            .ok_or_else(|| anyhow!("cannot read the next instruction."))?;

        let opcode = insn & 0x0000007f;
        let rs2 = (insn & 0x01f00000) >> 20;
        let funct3 = (insn & 0x00007000) >> 12;
        let funct7 = (insn & 0xfe000000) >> 25;

        self.mem.borrow_mut().watch_trigger = None;

        let opcode_cycle = get_opcode_cycle(insn)?;

        if opcode == 0b1110011 && funct3 == 0 && (rs2 == 0 || rs2 == 1) && funct7 == 0 {
            let res = self.ecall()?;
            self.hart_state.pc = res.0;
            let extra_cycle = res.2;

            self.session_cycle_count
                .borrow_mut()
                .callback_step(opcode_cycle, extra_cycle);

            if res.1.is_none() && self.mem.borrow_mut().watch_trigger.is_some() {
                let watch_result = self.mem.borrow_mut().watch_trigger.unwrap();
                return Ok(Some(ExitCode::HwWatchPoint((
                    watch_result.0,
                    watch_result.1,
                ))));
            } else {
                return Ok(res.1);
            }
        } else {
            let mut mem = self.mem.borrow_mut();
            let mut exec = InstructionExecutor {
                mem: mem.deref_mut(),
                hart_state: &mut self.hart_state,
            };
            exec.step().map_err(|err| {
                anyhow!(
                    "execution encounters an exception at 0x{:08x}. {err:?}",
                    self.hart_state.pc
                )
            })?;

            self.session_cycle_count
                .borrow_mut()
                .callback_step(opcode_cycle, 0);

            if mem.watch_trigger.is_some() {
                let watch_result = mem.watch_trigger.unwrap();
                return Ok(Some(ExitCode::HwWatchPoint((
                    watch_result.0,
                    watch_result.1,
                ))));
            }
        }

        Ok(None)
    }

    pub fn ecall(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        match self.hart_state.registers[crate::vm::reg_abi::REG_T0] {
            vm::ecall::HALT => self.ecall_halt(),
            vm::ecall::INPUT => self.ecall_input(),
            vm::ecall::SOFTWARE => self.ecall_software(),
            vm::ecall::SHA => self.ecall_sha(),
            vm::ecall::BIGINT => self.ecall_bigint(),
            ecall => bail!("Unknown ecall {ecall} at 0x{:08x}", self.hart_state.pc),
        }
    }

    pub fn ecall_halt(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        let tot_reg = self.hart_state.registers[crate::vm::reg_abi::REG_A0];
        let halt_type = tot_reg & 0xff;
        let user_exit = (tot_reg >> 8) & 0xff;

        match halt_type {
            crate::vm::halt::TERMINATE => {
                Ok((self.hart_state.pc, Some(ExitCode::Halted(user_exit)), 0))
            }
            crate::vm::halt::PAUSE => {
                Ok((self.hart_state.pc, Some(ExitCode::Paused(user_exit)), 0))
            }
            _ => bail!("Illegal halt type: {halt_type}"),
        }
    }

    pub fn ecall_input(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        Ok((self.hart_state.pc + 4, None, 0))
    }

    pub fn ecall_software(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        let to_guest_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A0];

        if ((to_guest_ptr as usize) < GUEST_MIN_MEM || (to_guest_ptr as usize) > GUEST_MAX_MEM)
            && to_guest_ptr != 0
        {
            bail!(
                "to_guest_ptr to 0x{:08x} of a SOFTWARE syscall at 0x{:08x} is invalid",
                to_guest_ptr,
                self.hart_state.pc
            );
        }

        let to_guest_words = self.hart_state.registers[crate::vm::reg_abi::REG_A1];
        let name_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A2];

        /// Align the given address `addr` upwards to alignment `align`.
        ///
        /// Requires that `align` is a power of two.
        const fn align_up(addr: usize, align: usize) -> usize {
            (addr + align - 1) & !(align - 1)
        }

        let chunks = align_up(to_guest_words as usize, 4);

        let syscall_name = {
            let mut addr = name_ptr;
            let mut s: Vec<u8> = Vec::new();
            loop {
                let bytes = self
                    .mem
                    .borrow_mut()
                    .read_mem(addr, MemAccessSize::Byte)
                    .ok_or_else(|| {
                        anyhow::format_err!("name_ptr of a SOFTWARE syscall cannot be read")
                    })? as u8;
                if bytes == 0 {
                    break;
                }
                s.push(bytes);
                addr += 1;
            }
            String::from_utf8(s).map_err(anyhow::Error::msg)?
        };

        let mut to_guest = vec![0; to_guest_words as usize];
        let exit_code = vm::syscall::handle_syscall(&syscall_name, &mut to_guest, self)?;
        if exit_code.is_some() {
            return Ok((self.hart_state.pc, None, 1 + chunks + 1));
        }

        if to_guest_ptr != 0 {
            let data: &[u8] = bytemuck::cast_slice(&to_guest);

            for i in 0..data.len() {
                let res = self.mem.borrow_mut().write_mem(
                    to_guest_ptr + i as u32,
                    MemAccessSize::Byte,
                    data[i] as u32,
                );
                if res == false {
                    bail!("cannot write the final hash for SHA.");
                }
            }
        }

        Ok((self.hart_state.pc + 4, None, 1 + chunks + 1))
    }

    pub fn ecall_sha(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        let out_state_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A0];
        let in_state_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A1];
        let mut block1_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A2];
        let mut block2_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A3];

        let count = self.hart_state.registers[crate::vm::reg_abi::REG_A4];

        let mut in_state = [0u8; 32];
        for i in 0..32 {
            let res = self
                .mem
                .borrow_mut()
                .read_mem(in_state_ptr + i as u32, MemAccessSize::Byte)
                .ok_or_else(|| anyhow!("cannot read the previous hash for SHA."))?;
            in_state[i] = res as u8;
        }
        let mut state: [u32; 8] = bytemuck::cast_slice(&in_state).try_into().unwrap();
        for word in &mut state {
            *word = word.to_be();
        }

        for _ in 0..count {
            let mut block = [0u32; 16];
            for i in 0..8 {
                block[i] = self
                    .mem
                    .borrow_mut()
                    .read_mem(block1_ptr + (i * 4) as u32, MemAccessSize::Word)
                    .ok_or_else(|| anyhow!("cannot read the input for SHA."))?;
            }
            for i in 0..8 {
                block[8 + i] = self
                    .mem
                    .borrow_mut()
                    .read_mem(block2_ptr + (i * 4) as u32, MemAccessSize::Word)
                    .ok_or_else(|| anyhow!("cannot read the input for SHA."))?;
            }
            sha2::compress256(
                &mut state,
                &[*GenericArray::from_slice(bytemuck::cast_slice(&block))],
            );

            block1_ptr += 64;
            block2_ptr += 64;
        }

        for word in &mut state {
            *word = u32::from_be(*word);
        }

        let out_state: [u8; 32] = bytemuck::cast_slice(&state).try_into().unwrap();
        for i in 0..32 {
            let res = self.mem.borrow_mut().write_mem(
                out_state_ptr + i as u32,
                MemAccessSize::Byte,
                out_state[i] as u32,
            );
            if res == false {
                bail!("cannot write the final hash for SHA.");
            }
        }

        Ok((self.hart_state.pc + 4, None, (73 * count) as usize))
    }

    pub fn ecall_bigint(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        let z_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A0];
        let op = self.hart_state.registers[crate::vm::reg_abi::REG_A1];
        let x_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A2];
        let y_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A3];
        let n_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A4];

        let load_bigint_le_bytes = |ptr: u32| -> Result<[u8; 32]> {
            let mut arr = [0u32; 8];
            for (i, word) in arr.iter_mut().enumerate() {
                *word = self
                    .mem
                    .borrow_mut()
                    .read_mem(ptr + (i * 4) as u32, MemAccessSize::Word)
                    .ok_or_else(|| anyhow!("cannot read the previous hash for BigInt."))?
                    .to_le();
            }
            Ok(bytemuck::cast(arr))
        };

        if op != 0 {
            bail!("ecall_bigint preflight: op must be set to 0");
        }

        let x = U256::from_le_bytes(load_bigint_le_bytes(x_ptr)?);
        let y = U256::from_le_bytes(load_bigint_le_bytes(y_ptr)?);
        let n = U256::from_le_bytes(load_bigint_le_bytes(n_ptr)?);

        // Compute modular multiplication, or simply multiplication if n == 0.
        let z: U256 = if n == U256::ZERO {
            x.checked_mul(&y)
                .expect("BigInt syscall requires non-overflowing multiplication when n = 0")
        } else {
            let (w_lo, w_hi) = x.mul_wide(&y);
            let w = w_hi.concat(&w_lo);
            let z = w.rem(&NonZero::<U512>::from_uint(n.resize()));
            z.resize()
        };

        // Store result.
        for (i, word) in bytemuck::cast::<_, [u32; 8]>(z.to_le_bytes())
            .into_iter()
            .enumerate()
        {
            let res = self.mem.borrow_mut().write_mem(
                z_ptr + (i * 4) as u32,
                MemAccessSize::Byte,
                word.to_le(),
            );
            if res == false {
                bail!("cannot write the final result for BigInt.");
            }
        }

        Ok((self.hart_state.pc + 4, None, 9))
    }
}
