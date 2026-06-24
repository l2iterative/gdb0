use crate::vm;
use crate::vm::bibc::{BigIntIO, Program};
use crate::vm::memory::{is_guest_memory, is_guest_region};
use crate::vm::session_cycle::{get_opcode_cycle, SessionCycleCount};
use crate::vm::ExitCode;
use anyhow::{anyhow, bail, Result};
use crypto_bigint::{CheckedMul, Encoding, NonZero, U256, U512};
use malachite::Natural;
use risc0_core::field::{baby_bear::BabyBearElem, Elem};
use risc0_zkp::core::{
    digest::DIGEST_WORDS,
    hash::poseidon2::{poseidon2_mix, CELLS},
};
use rrs_lib::instruction_executor::InstructionExecutor;
use rrs_lib::{HartState, MemAccessSize, Memory};
use sha2::digest::generic_array::GenericArray;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::ops::DerefMut;
use std::rc::Rc;

const BIGINT2_MAX_NONDET_PROGRAM_SIZE: u32 = 10 << 20;
const BIGINT2_WIDTH_BYTES: u32 = 16;
const BIGINT2_USER_START_ADDR: u32 = 0x0001_0000;
const BIGINT2_USER_END_ADDR: u32 = 0xbfff_0000;

pub struct Simulator {
    pub mem: Rc<RefCell<vm::memory::Memory>>,
    pub hart_state: HartState,
    pub machine_mode: u32,
    pub env: HashMap<String, String>,
    pub stdin: Cursor<Vec<u8>>,
    pub stdout: Cursor<Vec<u8>>,
    pub stderr: Cursor<Vec<u8>>,
    pub journal: Cursor<Vec<u8>>,
    pub args: Vec<String>,
    pub pipe_read_fds: HashMap<u32, Rc<RefCell<Cursor<Vec<u8>>>>>,
    pub pipe_write_fds: HashMap<u32, Rc<RefCell<Cursor<Vec<u8>>>>>,
    next_fd: u32,
    pub session_cycle_count: Rc<RefCell<SessionCycleCount>>,
}

struct BigInt2Io<'a> {
    vm: &'a mut Simulator,
    mode: u32,
}

impl BigInt2Io<'_> {
    fn arena_base(&self, arena: u32) -> Result<u32> {
        let arena = arena as usize;
        let base = self
            .vm
            .hart_state
            .registers
            .get(arena)
            .copied()
            .ok_or_else(|| anyhow!("invalid BIGINT2 arena register {arena}"))?;
        if base % 4 != 0 {
            bail!("unaligned BIGINT2 arena base 0x{base:08x}");
        }
        Ok(base)
    }

    fn check_addr(&self, addr: u32, count: u32) -> Result<()> {
        let len = count as usize;
        if !is_guest_region(addr, len) {
            bail!("invalid BIGINT2 guest region 0x{addr:08x}, len {count}");
        }

        let end = addr
            .checked_add(count)
            .ok_or_else(|| anyhow!("invalid BIGINT2 address range"))?;
        if addr < BIGINT2_USER_START_ADDR || (self.mode == 0 && end > BIGINT2_USER_END_ADDR) {
            bail!("invalid BIGINT2 address 0x{addr:08x}");
        }
        Ok(())
    }
}

impl BigIntIO for BigInt2Io<'_> {
    fn load(&mut self, arena: u32, offset: u32, count: u32) -> Result<Natural> {
        let base = self.arena_base(arena)?;
        let addr = base
            .checked_add(
                offset
                    .checked_mul(BIGINT2_WIDTH_BYTES)
                    .ok_or_else(|| anyhow!("BIGINT2 load offset overflow"))?,
            )
            .ok_or_else(|| anyhow!("BIGINT2 load address overflow"))?;
        self.check_addr(addr, count)?;

        let word_count = count.div_ceil(4);
        let mut limbs = Vec::with_capacity(word_count as usize);
        for i in 0..word_count {
            limbs.push(self.vm.read_guest_word(addr + i * 4)?);
        }

        if let Some(last_limb) = limbs.last_mut() {
            match count % 4 {
                1 => *last_limb &= 0x0000_00ff,
                2 => *last_limb &= 0x0000_ffff,
                3 => *last_limb &= 0x00ff_ffff,
                _ => {}
            }
        }

        Ok(Natural::from_limbs_asc(&limbs))
    }

    fn store(&mut self, arena: u32, offset: u32, count: u32, value: &Natural) -> Result<()> {
        if count as usize % BIGINT2_WIDTH_BYTES as usize != 0 {
            bail!("bigint_store: count ({count}) is not a multiple of {BIGINT2_WIDTH_BYTES}");
        }

        let base = self.arena_base(arena)?;
        let addr = base
            .checked_add(
                offset
                    .checked_mul(BIGINT2_WIDTH_BYTES)
                    .ok_or_else(|| anyhow!("BIGINT2 store offset overflow"))?,
            )
            .ok_or_else(|| anyhow!("BIGINT2 store address overflow"))?;
        self.check_addr(addr, count)?;

        let limbs = value.to_limbs_asc();
        if (count as usize) < limbs.len() * core::mem::size_of::<u32>() {
            bail!(
                "bigint_store: count ({count} bytes) too small for value ({} bytes)",
                limbs.len() * core::mem::size_of::<u32>()
            );
        }

        for i in 0..(count / 4) {
            let word = limbs.get(i as usize).copied().unwrap_or_default();
            self.vm.write_guest_word(addr + i * 4, word)?;
        }

        Ok(())
    }
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
            machine_mode: 0,
            env: env.clone(),
            stdin: Cursor::default(),
            stdout: Cursor::default(),
            stderr: Cursor::default(),
            journal: Cursor::default(),
            args: Vec::new(),
            pipe_read_fds: HashMap::new(),
            pipe_write_fds: HashMap::new(),
            next_fd: 4,
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

    pub(crate) fn read_fd_available(&self, read_fd: u32) -> Result<u32> {
        if read_fd == vm::fileno::STDIN {
            return Ok((self.stdin.get_ref().len() as u64 - self.stdin.position()) as u32);
        }

        if let Some(fd) = self.pipe_read_fds.get(&read_fd) {
            let fd = fd.borrow();
            return Ok((fd.get_ref().len() as u64 - fd.position()) as u32);
        }

        bail!("Bad read file descriptor {read_fd}");
    }

    pub(crate) fn read_from_fd(&mut self, read_fd: u32, buf: &mut [u8]) -> Result<usize> {
        if read_fd == vm::fileno::STDIN {
            return self
                .stdin
                .read(buf)
                .map_err(|err| anyhow!("cannot read from stdin. {err}"));
        }

        if let Some(fd) = self.pipe_read_fds.get(&read_fd) {
            return fd
                .borrow_mut()
                .read(buf)
                .map_err(|err| anyhow!("cannot read from pipe fd {read_fd}. {err}"));
        }

        bail!("Bad read file descriptor {read_fd}");
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

    pub(crate) fn write_to_fd(&mut self, write_fd: u32, data: &[u8]) -> Result<()> {
        if write_fd == vm::fileno::STDOUT
            || write_fd == vm::fileno::STDERR
            || write_fd == vm::fileno::JOURNAL
        {
            self.get_write_fd(write_fd)?
                .get_mut()
                .extend_from_slice(data);
            return Ok(());
        }

        if let Some(fd) = self.pipe_write_fds.get(&write_fd) {
            fd.borrow_mut().get_mut().extend_from_slice(data);
            return Ok(());
        }

        bail!("Bad write file descriptor {write_fd}");
    }

    fn read_guest_bytes(&mut self, ptr: u32, len: u32) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(len as usize);
        for i in 0..len {
            bytes.push(
                self.mem
                    .borrow_mut()
                    .read_mem(ptr + i, MemAccessSize::Byte)
                    .ok_or_else(|| anyhow!("guest memory at 0x{:08x} cannot be read", ptr + i))?
                    as u8,
            );
        }
        Ok(bytes)
    }

    fn read_guest_word(&mut self, ptr: u32) -> Result<u32> {
        if ptr % 4 != 0 {
            bail!("guest word at 0x{ptr:08x} is unaligned");
        }
        self.mem
            .borrow_mut()
            .read_mem(ptr, MemAccessSize::Word)
            .ok_or_else(|| anyhow!("guest word at 0x{ptr:08x} cannot be read"))
    }

    fn write_guest_word(&mut self, ptr: u32, word: u32) -> Result<()> {
        if ptr % 4 != 0 {
            bail!("guest word at 0x{ptr:08x} is unaligned");
        }
        if !self
            .mem
            .borrow_mut()
            .write_mem(ptr, MemAccessSize::Word, word)
        {
            bail!("guest word at 0x{ptr:08x} cannot be written");
        }
        Ok(())
    }

    fn write_guest_bytes(&mut self, ptr: u32, bytes: &[u8]) -> Result<()> {
        if !is_guest_region(ptr, bytes.len()) {
            bail!(
                "guest region at 0x{ptr:08x} with len {} is invalid",
                bytes.len()
            );
        }

        for (i, byte) in bytes.iter().enumerate() {
            let res =
                self.mem
                    .borrow_mut()
                    .write_mem(ptr + i as u32, MemAccessSize::Byte, *byte as u32);
            if res == false {
                bail!("guest memory at 0x{:08x} cannot be written", ptr + i as u32);
            }
        }

        Ok(())
    }

    pub(crate) fn allocate_pipe(&mut self) -> Result<(u32, u32)> {
        let read_fd = self.next_fd;
        let write_fd = read_fd
            .checked_add(1)
            .ok_or_else(|| anyhow!("file descriptor space exhausted"))?;
        self.next_fd = write_fd
            .checked_add(1)
            .ok_or_else(|| anyhow!("file descriptor space exhausted"))?;

        let pipe = Rc::new(RefCell::new(Cursor::new(Vec::new())));
        self.pipe_read_fds.insert(read_fd, pipe.clone());
        self.pipe_write_fds.insert(write_fd, pipe);
        Ok((read_fd, write_fd))
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

    pub(crate) fn get_pc(&self) -> u32 {
        self.hart_state.pc
    }

    pub(crate) fn set_pc(&mut self, pc: u32) {
        self.hart_state.pc = pc;
    }

    pub(crate) fn load_register(&self, idx: usize) -> Option<u32> {
        self.hart_state.registers.get(idx).copied()
    }

    pub(crate) fn store_register(&mut self, idx: usize, word: u32) -> bool {
        let Some(register) = self.hart_state.registers.get_mut(idx) else {
            return false;
        };
        *register = word;
        true
    }

    fn register_bank(addr: u32, len: usize) -> Option<u32> {
        let end = addr.checked_add(len as u32)?;
        for base in [vm::MACHINE_REGS_ADDR, vm::USER_REGS_ADDR] {
            let bank_end = base.checked_add(vm::REG_BANK_BYTES)?;
            if base <= addr && end <= bank_end {
                return Some(base);
            }
        }
        None
    }

    fn register_byte(&self, addr: u32) -> Option<u8> {
        let base = Self::register_bank(addr, 1)?;
        let offset = (addr - base) as usize;
        let idx = offset / core::mem::size_of::<u32>();
        let byte_idx = offset % core::mem::size_of::<u32>();
        Some(self.load_register(idx)?.to_le_bytes()[byte_idx])
    }

    fn write_register_byte(&mut self, addr: u32, byte: u8) -> bool {
        let Some(base) = Self::register_bank(addr, 1) else {
            return false;
        };
        let offset = (addr - base) as usize;
        let idx = offset / core::mem::size_of::<u32>();
        let byte_idx = offset % core::mem::size_of::<u32>();
        let Some(word) = self.load_register(idx) else {
            return false;
        };

        let mut bytes = word.to_le_bytes();
        bytes[byte_idx] = byte;
        self.store_register(idx, u32::from_le_bytes(bytes))
    }

    pub(crate) fn read_debug_mem(&mut self, addr: u32, size: MemAccessSize) -> Option<u32> {
        let len = match size {
            MemAccessSize::Byte => 1,
            MemAccessSize::HalfWord => 2,
            MemAccessSize::Word => 4,
        };

        if Self::register_bank(addr, len).is_some() {
            let mut bytes = [0u8; core::mem::size_of::<u32>()];
            for (i, byte) in bytes.iter_mut().enumerate().take(len) {
                *byte = self.register_byte(addr + i as u32)?;
            }
            return Some(u32::from_le_bytes(bytes));
        }

        self.mem
            .borrow_mut()
            .read_mem_with_privileges(addr, size, true)
    }

    pub(crate) fn write_debug_mem(&mut self, addr: u32, size: MemAccessSize, word: u32) -> bool {
        let len = match size {
            MemAccessSize::Byte => 1,
            MemAccessSize::HalfWord => 2,
            MemAccessSize::Word => 4,
        };

        if Self::register_bank(addr, len).is_some() {
            let bytes = word.to_le_bytes();
            for (i, byte) in bytes.iter().copied().enumerate().take(len) {
                if !self.write_register_byte(addr + i as u32, byte) {
                    return false;
                }
            }
            return true;
        }

        self.mem
            .borrow_mut()
            .write_mem_with_privileges(addr, size, word, true)
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
            vm::ecall::BIGINT2 => self.ecall_bigint2(0),
            vm::ecall::POSEIDON2 => self.ecall_poseidon2(),
            vm::ecall::USER => bail!("USER ecall is not implemented in this standalone VM"),
            _ => self.ecall_host(),
        }
    }

    pub fn ecall_host(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        match self.hart_state.registers[crate::vm::reg_abi::REG_A7] {
            vm::host_ecall::TERMINATE => self.ecall_host_terminate(),
            vm::host_ecall::READ => self.ecall_host_read(),
            vm::host_ecall::WRITE => self.ecall_host_write(),
            vm::host_ecall::SHA2 => self.ecall_host_sha2(),
            vm::host_ecall::POSEIDON2 => self.ecall_poseidon2(),
            vm::host_ecall::BIGINT => {
                self.ecall_bigint2(self.hart_state.registers[crate::vm::reg_abi::REG_T0])
            }
            host_ecall => bail!(
                "Unknown ecall {} / host ecall {} at 0x{:08x}",
                self.hart_state.registers[crate::vm::reg_abi::REG_T0],
                host_ecall,
                self.hart_state.pc
            ),
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
                Ok((self.hart_state.pc + 4, Some(ExitCode::Paused(user_exit)), 0))
            }
            crate::vm::halt::SPLIT => {
                Ok((self.hart_state.pc, Some(ExitCode::Paused(user_exit)), 0))
            }
            _ => bail!("Illegal halt type: {halt_type}"),
        }
    }

    pub fn ecall_input(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        Ok((self.hart_state.pc + 4, None, 0))
    }

    pub fn ecall_host_terminate(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        let user_exit = self.hart_state.registers[crate::vm::reg_abi::REG_A0] & 0xff;
        Ok((self.hart_state.pc + 4, Some(ExitCode::Halted(user_exit)), 0))
    }

    pub fn ecall_host_read(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        let fd = self.hart_state.registers[crate::vm::reg_abi::REG_A0];
        let ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A1];
        let len = self.hart_state.registers[crate::vm::reg_abi::REG_A2];

        if len > vm::MAX_IO_BYTES {
            bail!("Invalid length (too big) in host read: {len}");
        }
        if len > 0 && !is_guest_region(ptr, len as usize) {
            bail!("invalid host read guest region 0x{ptr:08x}, len {len}");
        }

        let mut bytes = vec![0u8; len as usize];
        let rlen = self.read_from_fd(fd, &mut bytes)?;
        self.write_guest_bytes(ptr, &bytes[..rlen])?;
        self.hart_state.registers[crate::vm::reg_abi::REG_A0] = rlen as u32;

        Ok((self.hart_state.pc + 4, None, 0))
    }

    pub fn ecall_host_write(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        let fd = self.hart_state.registers[crate::vm::reg_abi::REG_A0];
        let ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A1];
        let len = self.hart_state.registers[crate::vm::reg_abi::REG_A2];

        if len > vm::MAX_IO_BYTES {
            bail!("Invalid length (too big) in host write: {len}");
        }
        let bytes = self.read_guest_bytes(ptr, len)?;
        self.write_to_fd(fd, &bytes)?;
        self.hart_state.registers[crate::vm::reg_abi::REG_A0] = len;

        Ok((self.hart_state.pc + 4, None, 0))
    }

    pub fn ecall_host_sha2(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        let state_in_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A0];
        let state_out_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A1];
        let mut data_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A2];
        let count = self.hart_state.registers[crate::vm::reg_abi::REG_A3] & 0xffff;

        if count > 10 {
            bail!("Invalid count (too big) in host SHA2 ecall: {count}");
        }

        let mut in_state = [0u8; 32];
        for i in 0..32 {
            let res = self
                .mem
                .borrow_mut()
                .read_mem(state_in_ptr + i as u32, MemAccessSize::Byte)
                .ok_or_else(|| anyhow!("cannot read the previous hash for host SHA2."))?;
            in_state[i] = res as u8;
        }
        let mut state: [u32; 8] = bytemuck::cast_slice(&in_state).try_into().unwrap();
        for word in &mut state {
            *word = word.to_be();
        }

        for _ in 0..count {
            let mut block = [0u32; 16];
            for word in &mut block {
                *word = self
                    .mem
                    .borrow_mut()
                    .read_mem(data_ptr, MemAccessSize::Word)
                    .ok_or_else(|| anyhow!("cannot read the input for host SHA2."))?
                    .to_be();
                data_ptr += 4;
            }
            sha2::compress256(
                &mut state,
                &[*GenericArray::from_slice(bytemuck::cast_slice(&block))],
            );
        }

        for word in &mut state {
            *word = u32::from_be(*word);
        }

        self.write_guest_bytes(state_out_ptr, bytemuck::cast_slice(&state))?;

        Ok((self.hart_state.pc + 4, None, (73 * count) as usize))
    }

    pub fn ecall_poseidon2(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        let state_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A0];
        let mut in_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A1];
        let out_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A2];
        let bits_count = self.hart_state.registers[crate::vm::reg_abi::REG_A3];
        let is_elem = bits_count & vm::poseidon2::PFLAG_IS_ELEM != 0;
        let check_out = bits_count & vm::poseidon2::PFLAG_CHECK_OUT != 0;
        let count = bits_count & 0xffff;

        let mut state = [BabyBearElem::ZERO; CELLS];
        if state_ptr != 0 {
            for i in 0..DIGEST_WORDS {
                let word = self
                    .mem
                    .borrow_mut()
                    .read_mem(state_ptr + (i * 4) as u32, MemAccessSize::Word)
                    .ok_or_else(|| anyhow!("cannot read POSEIDON2 state."))?;
                state[DIGEST_WORDS * 2 + i] = word.into();
            }
        }

        for _ in 0..count {
            if is_elem {
                for cell in state.iter_mut().take(DIGEST_WORDS * 2) {
                    let word = self
                        .mem
                        .borrow_mut()
                        .read_mem(in_ptr, MemAccessSize::Word)
                        .ok_or_else(|| anyhow!("cannot read POSEIDON2 field input."))?;
                    *cell = word.into();
                    in_ptr += 4;
                }
            } else {
                for i in 0..DIGEST_WORDS {
                    let word = self
                        .mem
                        .borrow_mut()
                        .read_mem(in_ptr, MemAccessSize::Word)
                        .ok_or_else(|| anyhow!("cannot read POSEIDON2 packed input."))?;
                    state[2 * i] = (word & 0xffff).into();
                    state[2 * i + 1] = (word >> 16).into();
                    in_ptr += 4;
                }
            }
            poseidon2_mix(&mut state);
        }

        let mut digest = [0u32; DIGEST_WORDS];
        for (dst, elem) in digest.iter_mut().zip(state.iter()) {
            *dst = (*elem).into();
        }

        if check_out {
            for (i, expected) in digest.iter().enumerate() {
                let word = self
                    .mem
                    .borrow_mut()
                    .read_mem(out_ptr + (i * 4) as u32, MemAccessSize::Word)
                    .ok_or_else(|| anyhow!("cannot read POSEIDON2 expected output."))?;
                if word != *expected {
                    bail!("poseidon2 check failed: {word:#010x} != {expected:#010x}");
                }
            }
        } else {
            self.write_guest_bytes(out_ptr, bytemuck::cast_slice(&digest))?;
        }

        if state_ptr != 0 {
            let mut saved_state = [0u32; DIGEST_WORDS];
            for (dst, elem) in saved_state.iter_mut().zip(state[DIGEST_WORDS * 2..].iter()) {
                *dst = (*elem).into();
            }
            self.write_guest_bytes(state_ptr, bytemuck::cast_slice(&saved_state))?;
        }

        Ok((self.hart_state.pc + 4, None, count as usize))
    }

    pub fn ecall_bigint2(&mut self, mode: u32) -> Result<(u32, Option<ExitCode>, usize)> {
        if mode != 0 && mode != 1 {
            bail!("Invalid mode for BIGINT2 ecall: {mode}");
        }

        let blob_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A0];
        if blob_ptr % 4 != 0 {
            bail!("unaligned BIGINT2 blob pointer 0x{blob_ptr:08x}");
        }

        let nondet_program_size = self.read_guest_word(blob_ptr)?;
        let verify_program_size = self.read_guest_word(blob_ptr + 4)?;
        let consts_size = self.read_guest_word(blob_ptr + 8)?;
        let nondet_program_size_bytes = nondet_program_size
            .checked_mul(core::mem::size_of::<u32>() as u32)
            .ok_or_else(|| anyhow!("BIGINT2 nondet program size overflow"))?;
        if nondet_program_size_bytes > BIGINT2_MAX_NONDET_PROGRAM_SIZE {
            bail!("BIGINT2 nondet program is too large");
        }

        let default_nondet_ptr = blob_ptr
            .checked_add(4 * core::mem::size_of::<u32>() as u32)
            .ok_or_else(|| anyhow!("BIGINT2 nondet pointer overflow"))?;
        let nondet_program_ptr = match self.hart_state.registers[crate::vm::reg_abi::REG_T1] {
            0 => default_nondet_ptr,
            ptr => ptr,
        };
        let default_verify_ptr = nondet_program_ptr
            .checked_add(nondet_program_size_bytes)
            .ok_or_else(|| anyhow!("BIGINT2 verify pointer overflow"))?;
        let verify_program_ptr = match self.hart_state.registers[crate::vm::reg_abi::REG_T2] {
            0 => default_verify_ptr,
            ptr => ptr,
        };
        let verify_program_size_bytes = verify_program_size
            .checked_mul(core::mem::size_of::<u32>() as u32)
            .ok_or_else(|| anyhow!("BIGINT2 verify program size overflow"))?;
        let default_consts_ptr = verify_program_ptr
            .checked_add(verify_program_size_bytes)
            .ok_or_else(|| anyhow!("BIGINT2 consts pointer overflow"))?;
        let consts_ptr = match self.hart_state.registers[crate::vm::reg_abi::REG_T3] {
            0 => default_consts_ptr,
            ptr => ptr,
        };
        let consts_size_bytes = consts_size
            .checked_mul(core::mem::size_of::<u32>() as u32)
            .ok_or_else(|| anyhow!("BIGINT2 consts size overflow"))?;

        let program_bytes = self.read_guest_bytes(nondet_program_ptr, nondet_program_size_bytes)?;
        let mut cursor = Cursor::new(program_bytes);
        let program = Program::decode(&mut cursor)?;

        if verify_program_size_bytes != 0 {
            let verify_read_ptr = verify_program_ptr
                .checked_sub(core::mem::size_of::<u32>() as u32)
                .ok_or_else(|| anyhow!("BIGINT2 verify pointer underflow"))?;
            let _ = self.read_guest_bytes(verify_read_ptr, verify_program_size_bytes)?;
        }
        if consts_size_bytes != 0 {
            let _ = self.read_guest_bytes(consts_ptr, consts_size_bytes)?;
        }

        {
            let mut io = BigInt2Io { vm: self, mode };
            program.eval(&mut io)?;
        }

        Ok((
            self.hart_state.pc + 4,
            None,
            verify_program_size as usize + 1,
        ))
    }

    pub fn ecall_software(&mut self) -> Result<(u32, Option<ExitCode>, usize)> {
        let to_guest_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A0];
        let name_ptr = self.hart_state.registers[crate::vm::reg_abi::REG_A2];
        let syscall_id = self.hart_state.registers[crate::vm::reg_abi::REG_T6];

        /// Align the given address `addr` upwards to alignment `align`.
        ///
        /// Requires that `align` is a power of two.
        const fn align_up(addr: usize, align: usize) -> usize {
            (addr + align - 1) & !(align - 1)
        }

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

        let (syscall_kind, _current_abi) =
            vm::syscall::classify_syscall(&syscall_name, syscall_id)?;

        let to_guest_words = {
            if matches!(syscall_kind, vm::syscall::SyscallKind::Read) {
                let requested_bytes =
                    self.hart_state.registers[crate::vm::reg_abi::REG_A4] as usize;
                align_up(requested_bytes, core::mem::size_of::<u32>()) / core::mem::size_of::<u32>()
            } else {
                self.hart_state.registers[crate::vm::reg_abi::REG_A1] as usize
            }
        };

        let to_guest_bytes = to_guest_words * core::mem::size_of::<u32>();
        if to_guest_ptr != 0 && !is_guest_memory(to_guest_ptr) {
            bail!(
                "to_guest_ptr to 0x{:08x} of a SOFTWARE syscall at 0x{:08x} is invalid",
                to_guest_ptr,
                self.hart_state.pc
            );
        }

        if to_guest_ptr != 0 && !is_guest_region(to_guest_ptr, to_guest_bytes) {
            bail!(
                "to_guest region at 0x{:08x} of a SOFTWARE syscall at 0x{:08x} is invalid",
                to_guest_ptr,
                self.hart_state.pc
            );
        }

        let chunks = align_up(to_guest_words, 4);

        let mut to_guest = vec![0; to_guest_words];
        let exit_code =
            vm::syscall::handle_syscall(&syscall_name, syscall_id, &mut to_guest, self)?;
        if exit_code.is_some() {
            return Ok((self.hart_state.pc, exit_code, 1 + chunks + 1));
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
                MemAccessSize::Word,
                word.to_le(),
            );
            if res == false {
                bail!("cannot write the final result for BigInt.");
            }
        }

        Ok((self.hart_state.pc + 4, None, 9))
    }
}

impl vm::VmContext for Simulator {
    fn get_pc(&self) -> u32 {
        self.get_pc()
    }

    fn set_pc(&mut self, pc: u32) {
        self.set_pc(pc);
    }

    fn get_machine_mode(&self) -> u32 {
        self.machine_mode
    }

    fn set_machine_mode(&mut self, mode: u32) {
        self.machine_mode = mode;
    }

    fn load_register(&self, idx: usize) -> Option<u32> {
        self.load_register(idx)
    }

    fn store_register(&mut self, idx: usize, word: u32) -> bool {
        self.store_register(idx, word)
    }

    fn read_debug_mem(&mut self, addr: u32, size: MemAccessSize) -> Option<u32> {
        self.read_debug_mem(addr, size)
    }

    fn write_debug_mem(&mut self, addr: u32, size: MemAccessSize, word: u32) -> bool {
        self.write_debug_mem(addr, size, word)
    }

    fn add_hw_watchpoint(
        &mut self,
        addr: u32,
        len: u32,
        kind: gdbstub::target::ext::breakpoints::WatchKind,
    ) -> bool {
        if self
            .mem
            .borrow()
            .hw_watchpoints
            .contains(&(addr, len, kind))
        {
            false
        } else {
            self.mem.borrow_mut().hw_watchpoints.push((addr, len, kind));
            true
        }
    }

    fn remove_hw_watchpoint(
        &mut self,
        addr: u32,
        len: u32,
        kind: gdbstub::target::ext::breakpoints::WatchKind,
    ) -> bool {
        let idx = self
            .mem
            .borrow()
            .hw_watchpoints
            .iter()
            .position(|x| *x == (addr, len, kind));
        if let Some(idx) = idx {
            self.mem.borrow_mut().hw_watchpoints.remove(idx);
            true
        } else {
            false
        }
    }

    fn step(&mut self) -> Result<Option<ExitCode>> {
        self.step()
    }

    fn get_cycle_count(&self) -> u64 {
        self.session_cycle_count.borrow().get_session_cycle() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::memory::GUEST_MAX_MEM;
    use crate::vm::reg_abi::{
        REG_A0, REG_A1, REG_A2, REG_A3, REG_A4, REG_A7, REG_T0, REG_T1, REG_T2, REG_T3, REG_T6,
    };
    use rrs_lib::Memory;

    fn new_simulator() -> Simulator {
        let mem = Rc::new(RefCell::new(vm::memory::Memory::default()));
        Simulator::new(mem, 0x10000, &HashMap::new())
    }

    fn write_words(vm: &mut Simulator, ptr: u32, words: &[u32]) {
        for (i, word) in words.iter().enumerate() {
            assert!(vm.mem.borrow_mut().write_mem(
                ptr + (i * 4) as u32,
                MemAccessSize::Word,
                *word,
            ));
        }
    }

    fn read_words(vm: &mut Simulator, ptr: u32, len: usize) -> Vec<u32> {
        (0..len)
            .map(|i| {
                vm.mem
                    .borrow_mut()
                    .read_mem(ptr + (i * 4) as u32, MemAccessSize::Word)
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn debug_memory_exposes_current_register_windows() {
        let mut vm = new_simulator();
        vm.store_register(REG_A0, 0x1122_3344);
        vm.store_register(REG_A1, 0x5566_7788);

        let a0_user_addr = vm::USER_REGS_ADDR + (REG_A0 * core::mem::size_of::<u32>()) as u32;
        let a1_machine_addr = vm::MACHINE_REGS_ADDR + (REG_A1 * core::mem::size_of::<u32>()) as u32;

        assert_eq!(
            vm.read_debug_mem(a0_user_addr, MemAccessSize::Word),
            Some(0x1122_3344)
        );
        assert_eq!(
            vm.read_debug_mem(a0_user_addr + 1, MemAccessSize::HalfWord),
            Some(0x2233)
        );
        assert!(vm.write_debug_mem(a1_machine_addr + 2, MemAccessSize::Byte, 0xaa));
        assert_eq!(vm.load_register(REG_A1), Some(0x55aa_7788));
    }

    fn encode_bibc_op(code: u64, result_type: u64, a: u64, b: u64) -> u64 {
        code | (result_type << 4) | (a << 16) | (b << 40)
    }

    fn minimal_bigint2_copy_program() -> Vec<u8> {
        let mut program = Vec::new();
        program.extend_from_slice(b"bibc");
        program.extend_from_slice(&1u32.to_le_bytes());
        program.extend_from_slice(&0u32.to_le_bytes());
        program.extend_from_slice(&1u32.to_le_bytes());
        program.extend_from_slice(&0u32.to_le_bytes());
        program.extend_from_slice(&2u32.to_le_bytes());

        program.extend_from_slice(&16u64.to_le_bytes());
        program.extend_from_slice(&0u64.to_le_bytes());
        program.extend_from_slice(&0u64.to_le_bytes());
        program.extend_from_slice(&0u64.to_le_bytes());

        let load_a = ((REG_A1 as u64) << 16) | 0;
        let store_a = ((REG_A2 as u64) << 16) | 0;
        program.extend_from_slice(&encode_bibc_op(0x3, 0, load_a, 0).to_le_bytes());
        program.extend_from_slice(&encode_bibc_op(0x4, 0, store_a, 0).to_le_bytes());
        program
    }

    #[test]
    fn bigint_ecall_stores_full_words() {
        let mut vm = new_simulator();
        let z_ptr = 0x11000;
        let x_ptr = 0x12000;
        let y_ptr = 0x13000;
        let n_ptr = 0x14000;

        write_words(&mut vm, x_ptr, &[0x0102_0304, 0, 0, 0, 0, 0, 0, 0]);
        write_words(&mut vm, y_ptr, &[2, 0, 0, 0, 0, 0, 0, 0]);
        write_words(&mut vm, n_ptr, &[0, 0, 0, 0, 0, 0, 0, 0]);

        vm.hart_state.registers[REG_A0] = z_ptr;
        vm.hart_state.registers[REG_A1] = 0;
        vm.hart_state.registers[REG_A2] = x_ptr;
        vm.hart_state.registers[REG_A3] = y_ptr;
        vm.hart_state.registers[REG_A4] = n_ptr;

        vm.ecall_bigint().unwrap();
        let result = vm
            .mem
            .borrow_mut()
            .read_mem(z_ptr, MemAccessSize::Word)
            .unwrap();
        assert_eq!(result, 0x0204_0608);
    }

    #[test]
    fn bigint2_ecall_runs_current_bibc_program() {
        let mut vm = new_simulator();
        let blob_ptr = 0x11000;
        let program_ptr = 0x11010;
        let input_ptr = 0x12000;
        let output_ptr = 0x13000;
        let program = minimal_bigint2_copy_program();
        assert_eq!(program.len(), 18 * core::mem::size_of::<u32>());

        write_words(&mut vm, blob_ptr, &[18, 1, 0, 0]);
        vm.write_guest_bytes(program_ptr, &program).unwrap();
        write_words(
            &mut vm,
            input_ptr,
            &[0x0102_0304, 0x0506_0708, 0x1112_1314, 0x1516_1718],
        );

        vm.hart_state.pc = 0x10000;
        vm.hart_state.registers[REG_T0] = vm::ecall::BIGINT2;
        vm.hart_state.registers[REG_T1] = program_ptr;
        vm.hart_state.registers[REG_T2] = GUEST_MAX_MEM as u32;
        vm.hart_state.registers[REG_T3] = program_ptr + program.len() as u32;
        vm.hart_state.registers[REG_A0] = blob_ptr;
        vm.hart_state.registers[REG_A1] = input_ptr;
        vm.hart_state.registers[REG_A2] = output_ptr;

        let (pc, exit, cycles) = vm.ecall().unwrap();
        assert_eq!(pc, 0x10004);
        assert_eq!(exit, None);
        assert_eq!(cycles, 2);
        assert_eq!(
            read_words(&mut vm, output_ptr, 4),
            [0x0102_0304, 0x0506_0708, 0x1112_1314, 0x1516_1718]
        );
    }

    #[test]
    fn ecall_software_read_uses_byte_count_from_a4() {
        let mut vm = new_simulator();
        vm.write(vm::fileno::STDIN, b"hello").unwrap();

        let name_ptr = 0x12000;
        let to_guest_ptr = 0x11000;
        let read_name = b"risc0_zkvm_platform::syscall::nr::SYS_READ\0";
        for (i, byte) in read_name.iter().enumerate() {
            assert!(vm.mem.borrow_mut().write_mem(
                name_ptr + i as u32,
                MemAccessSize::Byte,
                *byte as u32
            ));
        }

        vm.hart_state.pc = 0x10000;
        vm.hart_state.registers[REG_T0] = vm::ecall::SOFTWARE;
        vm.hart_state.registers[REG_A0] = to_guest_ptr;
        vm.hart_state.registers[REG_A1] = 5;
        vm.hart_state.registers[REG_A2] = name_ptr;
        vm.hart_state.registers[REG_A3] = vm::fileno::STDIN;
        vm.hart_state.registers[REG_A4] = 5;
        vm.hart_state.registers[REG_T6] = vm::syscall_id::READ;

        let (pc, exit, cycles) = vm.ecall().unwrap();

        assert_eq!(pc, 0x10004);
        assert_eq!(exit, None);
        assert_eq!(cycles, 6);
        assert_eq!(vm.hart_state.registers[REG_A0], 5);
        assert_eq!(vm.hart_state.registers[REG_A1], 0x6f);

        let mut out = [0u8; 8];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = vm
                .mem
                .borrow_mut()
                .read_mem(to_guest_ptr + i as u32, MemAccessSize::Byte)
                .unwrap() as u8;
        }

        assert_eq!(&out[..5], b"hello");
        assert_eq!(&out[5..], &[0, 0, 0]);
    }

    #[test]
    fn ecall_software_read_returns_trailing_word_on_short_or_unaligned_reads() {
        let mut vm = new_simulator();
        vm.write(vm::fileno::STDIN, b"ab").unwrap();

        let name_ptr = 0x12000;
        let to_guest_ptr = 0x11000;
        let read_name = b"risc0_zkvm_platform::syscall::nr::SYS_READ\0";
        for (i, byte) in read_name.iter().enumerate() {
            assert!(vm.mem.borrow_mut().write_mem(
                name_ptr + i as u32,
                MemAccessSize::Byte,
                *byte as u32
            ));
        }

        vm.hart_state.pc = 0x10000;
        vm.hart_state.registers[REG_T0] = vm::ecall::SOFTWARE;
        vm.hart_state.registers[REG_A0] = to_guest_ptr;
        vm.hart_state.registers[REG_A1] = 5;
        vm.hart_state.registers[REG_A2] = name_ptr;
        vm.hart_state.registers[REG_A3] = vm::fileno::STDIN;
        vm.hart_state.registers[REG_A4] = 2;
        vm.hart_state.registers[REG_T6] = vm::syscall_id::READ;

        let (pc, exit, _) = vm.ecall().unwrap();

        assert_eq!(pc, 0x10004);
        assert_eq!(exit, None);
        assert_eq!(vm.hart_state.registers[REG_A0], 2);
        assert_eq!(
            vm.hart_state.registers[REG_A1],
            u32::from_le_bytes([b'a', b'b', 0, 0])
        );

        let mut out = [0u8; 4];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = vm
                .mem
                .borrow_mut()
                .read_mem(to_guest_ptr + i as u32, MemAccessSize::Byte)
                .unwrap() as u8;
        }
        assert_eq!(&out, b"ab\0\0");
    }

    #[test]
    fn host_read_ecall_reads_into_guest_memory() {
        let mut vm = new_simulator();
        let ptr = 0x11000;
        vm.write(vm::fileno::STDIN, b"abcdef").unwrap();

        vm.hart_state.pc = 0x10000;
        vm.hart_state.registers[crate::vm::reg_abi::REG_T0] = 99;
        vm.hart_state.registers[crate::vm::reg_abi::REG_A7] = vm::host_ecall::READ;
        vm.hart_state.registers[REG_A0] = vm::fileno::STDIN;
        vm.hart_state.registers[REG_A1] = ptr;
        vm.hart_state.registers[REG_A2] = 4;

        let (pc, exit, _) = vm.ecall().unwrap();
        assert_eq!(pc, 0x10004);
        assert_eq!(exit, None);
        assert_eq!(vm.hart_state.registers[REG_A0], 4);

        let got = vm
            .mem
            .borrow_mut()
            .read_mem(ptr, MemAccessSize::Word)
            .unwrap();
        assert_eq!(got, u32::from_le_bytes(*b"abcd"));
    }

    #[test]
    fn host_write_ecall_writes_from_guest_memory() {
        let mut vm = new_simulator();
        let ptr = 0x11000;
        write_words(&mut vm, ptr, &[u32::from_le_bytes(*b"wxyz")]);

        vm.hart_state.pc = 0x10000;
        vm.hart_state.registers[crate::vm::reg_abi::REG_T0] = 99;
        vm.hart_state.registers[crate::vm::reg_abi::REG_A7] = vm::host_ecall::WRITE;
        vm.hart_state.registers[REG_A0] = vm::fileno::STDOUT;
        vm.hart_state.registers[REG_A1] = ptr;
        vm.hart_state.registers[REG_A2] = 4;

        let (pc, exit, _) = vm.ecall().unwrap();
        assert_eq!(pc, 0x10004);
        assert_eq!(exit, None);
        assert_eq!(vm.hart_state.registers[REG_A0], 4);

        let mut stdout = Vec::new();
        vm.read_to_end(vm::fileno::STDOUT, &mut stdout).unwrap();
        assert_eq!(stdout, b"wxyz");
    }

    #[test]
    fn host_sha2_ecall_compresses_contiguous_blocks() {
        let mut vm = new_simulator();
        let state_in = 0x11000;
        let state_out = 0x12000;
        let data = 0x13000;

        write_words(
            &mut vm,
            state_in,
            &[
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
        );
        write_words(&mut vm, data, &[0u32; 16]);

        vm.hart_state.pc = 0x10000;
        vm.hart_state.registers[crate::vm::reg_abi::REG_T0] = 99;
        vm.hart_state.registers[crate::vm::reg_abi::REG_A7] = vm::host_ecall::SHA2;
        vm.hart_state.registers[REG_A0] = state_in;
        vm.hart_state.registers[REG_A1] = state_out;
        vm.hart_state.registers[REG_A2] = data;
        vm.hart_state.registers[REG_A3] = 1;

        let (pc, exit, _) = vm.ecall().unwrap();
        assert_eq!(pc, 0x10004);
        assert_eq!(exit, None);

        let mut expected_state = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        for word in &mut expected_state {
            *word = u32::to_be(*word);
        }
        let mut expected_block = [0u32; 16];
        for word in &mut expected_block {
            *word = u32::to_be(*word);
        }
        sha2::compress256(
            &mut expected_state,
            &[*GenericArray::from_slice(bytemuck::cast_slice(
                &expected_block,
            ))],
        );
        for word in &mut expected_state {
            *word = u32::from_be(*word);
        }

        let mut actual = [0u32; 8];
        for (i, word) in actual.iter_mut().enumerate() {
            *word = vm
                .mem
                .borrow_mut()
                .read_mem(state_out + (i * 4) as u32, MemAccessSize::Word)
                .unwrap();
        }
        assert_eq!(actual, expected_state);
    }

    #[test]
    fn poseidon2_ecall_matches_current_vector_shape() {
        let mut vm = new_simulator();
        let state_ptr = 0x11000;
        let in_ptr = 0x12000;
        let out_ptr = 0x13000;
        let input: Vec<u32> = (0..16).collect();
        let saved_state: Vec<u32> = (16..24).collect();

        write_words(&mut vm, in_ptr, &input);
        write_words(&mut vm, state_ptr, &saved_state);

        vm.hart_state.pc = 0x10000;
        vm.hart_state.registers[crate::vm::reg_abi::REG_T0] = vm::ecall::POSEIDON2;
        vm.hart_state.registers[REG_A7] = 0;
        vm.hart_state.registers[REG_A0] = state_ptr;
        vm.hart_state.registers[REG_A1] = in_ptr;
        vm.hart_state.registers[REG_A2] = out_ptr;
        vm.hart_state.registers[REG_A3] = vm::poseidon2::PFLAG_IS_ELEM | 1;

        let (pc, exit, cycles) = vm.ecall().unwrap();
        assert_eq!(pc, 0x10004);
        assert_eq!(exit, None);
        assert_eq!(cycles, 1);

        let expected = [
            0x2ed3e23d, 0x12921fb0, 0x0e659e79, 0x61d81dc9, 0x32bae33b, 0x62486ae3, 0x1e681b60,
            0x24b91325, 0x2a2ef5b9, 0x50e8593e, 0x5bc818ec, 0x10691997, 0x35a14520, 0x2ba6a3c5,
            0x279d47ec, 0x55014e81, 0x5953a67f, 0x2f403111, 0x6b8828ff, 0x1801301f, 0x2749207a,
            0x3dc9cf21, 0x3c985ba2, 0x57a99864,
        ];

        assert_eq!(
            read_words(&mut vm, out_ptr, DIGEST_WORDS),
            expected[..DIGEST_WORDS]
        );
        assert_eq!(
            read_words(&mut vm, state_ptr, DIGEST_WORDS),
            expected[DIGEST_WORDS * 2..]
        );
    }
}
