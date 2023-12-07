use crate::vm::memory::GUEST_MAX_MEM;
use anyhow::{anyhow, bail, Context, Result};
use elf::endian::LittleEndian;
use elf::file::Class;
use elf::ElfBytes;
use rrs_lib::{MemAccessSize, Memory};
use std::cell::RefCell;
use std::rc::Rc;

// This file is basically a cherry-pick from https://github.com/risc0/risc0/blob/main/risc0/binfmt/src/elf.rs#L34

pub fn load_elf<M: Memory>(mem: Rc<RefCell<M>>, input: &[u8]) -> Result<u32> {
    let elf = ElfBytes::<LittleEndian>::minimal_parse(input)
        .map_err(|err| anyhow!("Elf parse error: {err}"))?;

    if elf.ehdr.class != Class::ELF32 {
        bail!("Not a 32-bit ELF");
    }
    if elf.ehdr.e_machine != elf::abi::EM_RISCV {
        bail!("Invalid machine type, must be RISC-V");
    }
    if elf.ehdr.e_type != elf::abi::ET_EXEC {
        bail!("Invalid ELF type, must be executable");
    }

    let entry: u32 = elf
        .ehdr
        .e_entry
        .try_into()
        .map_err(|err| anyhow!("e_entry was larger than 32 bits. {err}"))?;
    if entry >= (GUEST_MAX_MEM as u32) || entry % 4 != 0 {
        bail!("Invalid entrypoint");
    }

    let segments = elf.segments().ok_or(anyhow!("Missing segment table"))?;
    if segments.len() > 256 {
        bail!("Too many program headers");
    }

    for segment in segments.iter().filter(|x| x.p_type == elf::abi::PT_LOAD) {
        let file_size: u32 = segment
            .p_filesz
            .try_into()
            .map_err(|err| anyhow!("filesize was larger than 32 bits. {err}"))?;
        if file_size >= GUEST_MAX_MEM as u32 {
            bail!("Invalid segment file_size");
        }
        let mem_size: u32 = segment
            .p_memsz
            .try_into()
            .map_err(|err| anyhow!("mem_size was larger than 32 bits {err}"))?;
        if mem_size >= GUEST_MAX_MEM as u32 {
            bail!("Invalid segment mem_size");
        }
        let vaddr: u32 = segment
            .p_vaddr
            .try_into()
            .map_err(|err| anyhow!("vaddr is larger than 32 bits. {err}"))?;
        if vaddr % 4 != 0 {
            bail!("vaddr {vaddr:08x} is unaligned");
        }
        let offset: u32 = segment
            .p_offset
            .try_into()
            .map_err(|err| anyhow!("offset is larger than 32 bits. {err}"))?;
        for i in (0..mem_size).step_by(4) {
            let addr = vaddr.checked_add(i).context("Invalid segment vaddr")?;
            if addr >= GUEST_MAX_MEM as u32 {
                bail!("Address [0x{addr:08x}] exceeds maximum address for guest programs [0x{GUEST_MAX_MEM:08x}]");
            }
            if i >= file_size {
                // Past the file size, all zeros.
                mem.borrow_mut().write_mem(addr, MemAccessSize::Word, 0);
            } else {
                let mut word = 0;
                // Don't read past the end of the file.
                let len = core::cmp::min(file_size - i, 4);
                for j in 0..len {
                    let offset = (offset + i + j) as usize;
                    let byte = input.get(offset).context("Invalid segment offset")?;
                    word |= (*byte as u32) << (j * 8);
                }
                mem.borrow_mut().write_mem(addr, MemAccessSize::Word, word);
            }
        }
    }

    Ok(entry)
}
