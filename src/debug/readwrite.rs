use gdbstub::arch::Arch;
use gdbstub::target::ext::base::single_register_access::{SingleRegisterAccess, SingleRegisterAccessOps};
use gdbstub::target::ext::base::singlethread::{SingleThreadBase, SingleThreadResumeOps};
use gdbstub::target::{TargetError, TargetResult};
use gdbstub_arch::riscv::reg::id::RiscvRegId;
use rrs_lib::{MemAccessSize, Memory};
use crate::debug::debugger::Debugger;
use crate::vm::memory::{GUEST_MAX_MEM, GUEST_MIN_MEM};

impl SingleThreadBase for Debugger {
    fn read_registers(
        &mut self,
        regs: &mut <Self::Arch as Arch>::Registers,
    ) -> TargetResult<(), Self> {
        regs.x = self.simulator.borrow().hart_state.registers;
        regs.pc = self.simulator.borrow().hart_state.pc;
        Ok(())
    }

    fn write_registers(
        &mut self,
        regs: &<Self::Arch as Arch>::Registers,
    ) -> TargetResult<(), Self> {
        self.simulator.borrow_mut().hart_state.registers = regs.x;
        self.simulator.borrow_mut().hart_state.pc = regs.pc;
        Ok(())
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }

    fn read_addrs(
        &mut self,
        start_addr: <Self::Arch as Arch>::Usize,
        data: &mut [u8],
    ) -> TargetResult<usize, Self> {
        if !(GUEST_MIN_MEM..GUEST_MAX_MEM).contains(&(start_addr as usize)) {
            return Err(TargetError::NonFatal);
        }

        let end_addr = std::cmp::min(start_addr + data.len() as u32, GUEST_MAX_MEM as u32);

        for (addr, val) in (start_addr..end_addr).zip(data.iter_mut()) {
            *val = self
                .simulator
                .borrow_mut()
                .mem
                .borrow_mut()
                .read_mem(addr, MemAccessSize::Byte)
                .ok_or(TargetError::NonFatal)? as u8;
        }

        Ok((end_addr - start_addr) as usize)
    }

    fn write_addrs(
        &mut self,
        start_addr: <Self::Arch as Arch>::Usize,
        data: &[u8],
    ) -> TargetResult<(), Self> {
        for (addr, val) in (start_addr..).zip(data.iter().copied()) {
            let res = self.simulator.borrow_mut().mem.borrow_mut().write_mem(
                addr,
                MemAccessSize::Byte,
                val as u32,
            );
            if res == false {
                return Err(TargetError::NonFatal);
            }
        }
        Ok(())
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl SingleRegisterAccess<()> for Debugger {
    fn read_register(
        &mut self,
        _tid: (),
        reg_id: <Self::Arch as Arch>::RegId,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        return match reg_id {
            RiscvRegId::Gpr(idx) => {
                buf.copy_from_slice(
                    &self.simulator.borrow_mut().hart_state.registers[idx as usize].to_le_bytes(),
                );
                Ok(buf.len())
            }
            RiscvRegId::Fpr(_) => Err(TargetError::NonFatal),
            RiscvRegId::Pc => {
                buf.copy_from_slice(&self.simulator.borrow_mut().hart_state.pc.to_le_bytes());
                Ok(buf.len())
            }
            RiscvRegId::Csr(_) => Err(TargetError::NonFatal),
            RiscvRegId::Priv => Err(TargetError::NonFatal),
            RiscvRegId::_Marker(_) => Err(TargetError::NonFatal),
            _ => Err(TargetError::NonFatal),
        };
    }

    fn write_register(
        &mut self,
        _tid: (),
        reg_id: <Self::Arch as Arch>::RegId,
        val: &[u8],
    ) -> TargetResult<(), Self> {
        return match reg_id {
            RiscvRegId::Gpr(idx) => {
                self.simulator.borrow_mut().hart_state.registers[idx as usize] =
                    u32::from_le_bytes([val[0], val[1], val[2], val[3]]);
                Ok(())
            }
            RiscvRegId::Fpr(_) => Err(TargetError::NonFatal),
            RiscvRegId::Pc => {
                self.simulator.borrow_mut().hart_state.pc =
                    u32::from_le_bytes([val[0], val[1], val[2], val[3]]);
                Ok(())
            }
            RiscvRegId::Csr(_) => Err(TargetError::NonFatal),
            RiscvRegId::Priv => Err(TargetError::NonFatal),
            RiscvRegId::_Marker(_) => Err(TargetError::NonFatal),
            _ => Err(TargetError::NonFatal),
        };
    }
}