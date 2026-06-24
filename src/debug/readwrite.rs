use crate::debug::debugger::Debugger;
use crate::vm::reg_abi;
use gdbstub::arch::Arch;
use gdbstub::target::ext::base::single_register_access::{
    SingleRegisterAccess, SingleRegisterAccessOps,
};
use gdbstub::target::ext::base::singlethread::{SingleThreadBase, SingleThreadResumeOps};
use gdbstub::target::{TargetError, TargetResult};
use gdbstub_arch::riscv::reg::id::RiscvRegId;
use rrs_lib::MemAccessSize;

impl SingleThreadBase for Debugger {
    fn read_registers(
        &mut self,
        regs: &mut <Self::Arch as Arch>::Registers,
    ) -> TargetResult<(), Self> {
        let sim = self.simulator.borrow();
        for idx in 0..reg_abi::REG_MAX {
            regs.x[idx] = sim.load_register(idx).ok_or(TargetError::NonFatal)?;
        }
        regs.pc = sim.get_pc();
        Ok(())
    }

    fn write_registers(
        &mut self,
        regs: &<Self::Arch as Arch>::Registers,
    ) -> TargetResult<(), Self> {
        let mut sim = self.simulator.borrow_mut();
        for (idx, word) in regs.x.iter().copied().enumerate() {
            if !sim.store_register(idx, word) {
                return Err(TargetError::NonFatal);
            }
        }
        sim.set_pc(regs.pc);
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
        let mut sim = self.simulator.borrow_mut();
        let mut read = 0;
        for (offset, val) in data.iter_mut().enumerate() {
            let Some(addr) = start_addr.checked_add(offset as u32) else {
                break;
            };
            let Some(byte) = sim.read_debug_mem(addr, MemAccessSize::Byte) else {
                if read == 0 {
                    return Err(TargetError::NonFatal);
                }
                break;
            };
            *val = byte as u8;
            read += 1;
        }

        Ok(read)
    }

    fn write_addrs(
        &mut self,
        start_addr: <Self::Arch as Arch>::Usize,
        data: &[u8],
    ) -> TargetResult<(), Self> {
        let mut sim = self.simulator.borrow_mut();
        for (offset, val) in data.iter().copied().enumerate() {
            let Some(addr) = start_addr.checked_add(offset as u32) else {
                return Err(TargetError::NonFatal);
            };
            if !sim.write_debug_mem(addr, MemAccessSize::Byte, val as u32) {
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
                let word = self
                    .simulator
                    .borrow()
                    .load_register(idx as usize)
                    .ok_or(TargetError::NonFatal)?;
                buf.copy_from_slice(&word.to_le_bytes());
                Ok(buf.len())
            }
            RiscvRegId::Fpr(_) => Err(TargetError::NonFatal),
            RiscvRegId::Pc => {
                buf.copy_from_slice(&self.simulator.borrow().get_pc().to_le_bytes());
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
                let word = u32::from_le_bytes([val[0], val[1], val[2], val[3]]);
                if !self
                    .simulator
                    .borrow_mut()
                    .store_register(idx as usize, word)
                {
                    return Err(TargetError::NonFatal);
                }
                Ok(())
            }
            RiscvRegId::Fpr(_) => Err(TargetError::NonFatal),
            RiscvRegId::Pc => {
                self.simulator
                    .borrow_mut()
                    .set_pc(u32::from_le_bytes([val[0], val[1], val[2], val[3]]));
                Ok(())
            }
            RiscvRegId::Csr(_) => Err(TargetError::NonFatal),
            RiscvRegId::Priv => Err(TargetError::NonFatal),
            RiscvRegId::_Marker(_) => Err(TargetError::NonFatal),
            _ => Err(TargetError::NonFatal),
        };
    }
}
