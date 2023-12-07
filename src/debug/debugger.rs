use crate::vm::memory::{GUEST_MAX_MEM, GUEST_MIN_MEM};
use crate::vm::simulator::Simulator;
use crate::vm::ExitCode;
use alloc::rc::Rc;
use gdbstub::arch::Arch;
use gdbstub::common::{Pid, Signal};
use gdbstub::conn::{Connection, ConnectionExt};
use gdbstub::stub::run_blocking::{Event, WaitForStopReasonError};
use gdbstub::stub::{run_blocking, SingleThreadStopReason};
use gdbstub::target::ext::base::reverse_exec::{ReverseContOps, ReverseStepOps};
use gdbstub::target::ext::base::single_register_access::{
    SingleRegisterAccess, SingleRegisterAccessOps,
};
use gdbstub::target::ext::base::singlethread::{
    SingleThreadBase, SingleThreadRangeStepping, SingleThreadRangeSteppingOps, SingleThreadResume,
    SingleThreadResumeOps, SingleThreadSingleStep, SingleThreadSingleStepOps,
};
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::breakpoints::{
    Breakpoints, BreakpointsOps, HwWatchpoint, HwWatchpointOps, SwBreakpoint, SwBreakpointOps,
    WatchKind,
};
use gdbstub::target::ext::exec_file::{ExecFile, ExecFileOps};
use gdbstub::target::ext::host_io::{
    FsKind, HostIo, HostIoClose, HostIoCloseOps, HostIoErrno, HostIoError, HostIoFstat,
    HostIoFstatOps, HostIoOpen, HostIoOpenFlags, HostIoOpenMode, HostIoOpenOps, HostIoOps,
    HostIoPread, HostIoPreadOps, HostIoReadlink, HostIoReadlinkOps, HostIoResult, HostIoSetfs,
    HostIoSetfsOps, HostIoStat,
};
use gdbstub::target::{Target, TargetError, TargetResult};
use gdbstub_arch::riscv::reg::id::RiscvRegId;
use rrs_lib::{MemAccessSize, Memory};
use std::cell::RefCell;
use std::collections::HashSet;

#[derive(Eq, PartialEq)]
pub enum ExecMode {
    Step,
    Continue,
    RangeStep(u32, u32),
    Interrupted,
}

pub struct Debugger {
    pub elf: Vec<u8>,
    pub simulator: Rc<RefCell<Simulator>>,
    pub exec_mode: ExecMode,
    pub breakpoints: HashSet<u32>,
}

impl Target for Debugger {
    type Arch = gdbstub_arch::riscv::Riscv32;
    type Error = &'static str;

    fn base_ops(&mut self) -> BaseOps<'_, Self::Arch, Self::Error> {
        BaseOps::SingleThread(self)
    }

    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }

    fn support_exec_file(&mut self) -> Option<ExecFileOps<'_, Self>> {
        Some(self)
    }

    fn support_host_io(&mut self) -> Option<HostIoOps<'_, Self>> {
        Some(self)
    }
}

impl Breakpoints for Debugger {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_watchpoint(&mut self) -> Option<HwWatchpointOps<'_, Self>> {
        Some(self)
    }
}

impl SwBreakpoint for Debugger {
    fn add_sw_breakpoint(
        &mut self,
        addr: <Self::Arch as Arch>::Usize,
        _kind: <Self::Arch as Arch>::BreakpointKind,
    ) -> TargetResult<bool, Self> {
        Ok(self.breakpoints.insert(addr))
    }

    fn remove_sw_breakpoint(
        &mut self,
        addr: <Self::Arch as Arch>::Usize,
        _kind: <Self::Arch as Arch>::BreakpointKind,
    ) -> TargetResult<bool, Self> {
        Ok(self.breakpoints.remove(&addr))
    }
}

impl HwWatchpoint for Debugger {
    fn add_hw_watchpoint(
        &mut self,
        addr: <Self::Arch as Arch>::Usize,
        len: <Self::Arch as Arch>::Usize,
        kind: WatchKind,
    ) -> TargetResult<bool, Self> {
        let sim_ref = self.simulator.borrow_mut();
        let hw_wp_ref = &mut sim_ref.mem.borrow_mut().hw_watchpoints;
        if hw_wp_ref.contains(&(addr, len, kind)) {
            Ok(false)
        } else {
            hw_wp_ref.push((addr, len, kind));
            Ok(true)
        }
    }

    fn remove_hw_watchpoint(
        &mut self,
        addr: <Self::Arch as Arch>::Usize,
        len: <Self::Arch as Arch>::Usize,
        kind: WatchKind,
    ) -> TargetResult<bool, Self> {
        let sim_ref = self.simulator.borrow_mut();
        let hw_wp_ref = &mut sim_ref.mem.borrow_mut().hw_watchpoints;

        let idx = hw_wp_ref.iter().position(|x| *x == (addr, len, kind));
        if idx.is_none() {
            Ok(false)
        } else {
            hw_wp_ref.remove(idx.unwrap());
            Ok(true)
        }
    }
}

impl ExecFile for Debugger {
    fn get_exec_file(
        &self,
        _pid: Option<Pid>,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let filename = b"/r0code.elf";
        Ok(crate::debug::copy_range_to_buf(
            filename, offset, length, buf,
        ))
    }
}
impl HostIo for Debugger {
    fn support_open(&mut self) -> Option<HostIoOpenOps<'_, Self>> {
        Some(self)
    }

    fn support_close(&mut self) -> Option<HostIoCloseOps<'_, Self>> {
        Some(self)
    }

    fn support_pread(&mut self) -> Option<HostIoPreadOps<'_, Self>> {
        Some(self)
    }

    fn support_fstat(&mut self) -> Option<HostIoFstatOps<'_, Self>> {
        Some(self)
    }

    fn support_readlink(&mut self) -> Option<HostIoReadlinkOps<'_, Self>> {
        Some(self)
    }

    fn support_setfs(&mut self) -> Option<HostIoSetfsOps<'_, Self>> {
        Some(self)
    }
}

impl HostIoOpen for Debugger {
    fn open(
        &mut self,
        filename: &[u8],
        _flags: HostIoOpenFlags,
        _mode: HostIoOpenMode,
    ) -> HostIoResult<u32, Self> {
        if filename == b"/r0code.elf" {
            return Ok(0);
        }
        return Err(HostIoError::Errno(HostIoErrno::ENOENT));
    }
}

impl HostIoClose for Debugger {
    fn close(&mut self, _fd: u32) -> HostIoResult<(), Self> {
        return Ok(());
    }
}

impl HostIoPread for Debugger {
    fn pread(
        &mut self,
        fd: u32,
        count: usize,
        offset: u64,
        buf: &mut [u8],
    ) -> HostIoResult<usize, Self> {
        return if fd == 0 {
            Ok(crate::debug::copy_range_to_buf(
                &self.elf, offset, count, buf,
            ))
        } else {
            Err(HostIoError::Errno(HostIoErrno::EBADF))
        };
    }
}

impl HostIoFstat for Debugger {
    fn fstat(&mut self, fd: u32) -> HostIoResult<HostIoStat, Self> {
        if fd == 0 {
            return Ok(HostIoStat {
                st_dev: 0,
                st_ino: 0,
                st_mode: HostIoOpenMode::empty(),
                st_nlink: 0,
                st_uid: 0,
                st_gid: 0,
                st_rdev: 0,
                st_size: self.elf.len() as u64,
                st_blksize: 0,
                st_blocks: 0,
                st_atime: 0,
                st_mtime: 0,
                st_ctime: 0,
            });
        } else {
            return Err(HostIoError::Errno(HostIoErrno::EBADF));
        }
    }
}

impl HostIoSetfs for Debugger {
    fn setfs(&mut self, _fs: FsKind) -> HostIoResult<(), Self> {
        Ok(())
    }
}

impl HostIoReadlink for Debugger {
    fn readlink(&mut self, filename: &[u8], buf: &mut [u8]) -> HostIoResult<usize, Self> {
        return if filename == b"/proc/1/exe" {
            // Support `info proc exe` command
            let exe = b"/r0code.elf";
            Ok(crate::debug::copy_to_buf(exe, buf))
        } else if filename == b"/proc/1/cwd" {
            // Support `info proc cwd` command
            let cwd = b"/";
            Ok(crate::debug::copy_to_buf(cwd, buf))
        } else {
            Err(HostIoError::Errno(HostIoErrno::ENOENT))
        };
    }
}

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

impl SingleThreadResume for Debugger {
    fn resume(&mut self, signal: Option<Signal>) -> Result<(), Self::Error> {
        if signal.is_some() {
            return Err("no support for continuing with signal");
        }
        self.exec_mode = ExecMode::Continue;
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }

    fn support_range_step(&mut self) -> Option<SingleThreadRangeSteppingOps<'_, Self>> {
        Some(self)
    }

    fn support_reverse_step(&mut self) -> Option<ReverseStepOps<'_, (), Self>> {
        None
    }

    fn support_reverse_cont(&mut self) -> Option<ReverseContOps<'_, (), Self>> {
        None
    }
}

impl SingleThreadSingleStep for Debugger {
    fn step(&mut self, signal: Option<Signal>) -> Result<(), Self::Error> {
        if signal.is_some() {
            return Err("no support for stepping with signal");
        }
        self.exec_mode = ExecMode::Step;
        Ok(())
    }
}

impl SingleThreadRangeStepping for Debugger {
    fn resume_range_step(
        &mut self,
        start: <Self::Arch as Arch>::Usize,
        end: <Self::Arch as Arch>::Usize,
    ) -> Result<(), Self::Error> {
        self.exec_mode = ExecMode::RangeStep(start, end);
        Ok(())
    }
}

impl run_blocking::BlockingEventLoop for Debugger {
    type Target = Self;
    type Connection = Box<dyn ConnectionExt<Error = std::io::Error>>;
    type StopReason = SingleThreadStopReason<u32>;

    fn wait_for_stop_reason(
        target: &mut Self::Target,
        conn: &mut Self::Connection,
    ) -> Result<
        Event<Self::StopReason>,
        WaitForStopReasonError<
            <Self::Target as Target>::Error,
            <Self::Connection as Connection>::Error,
        >,
    > {
        let mut poll_incoming_data = || {
            // gdbstub takes ownership of the underlying connection, so the `borrow_conn`
            // method is used to borrow the underlying connection back from the stub to
            // check for incoming data.
            conn.peek().map(|b| b.is_some()).unwrap_or(true)
        };

        match target.exec_mode {
            ExecMode::Step => {
                if poll_incoming_data() {
                    let byte = conn
                        .read()
                        .map_err(run_blocking::WaitForStopReasonError::Connection)?;
                    return Ok(Event::IncomingData(byte));
                }

                let res = target.simulator.borrow_mut().step();
                if res.is_err() {
                    match res {
                        Ok(_) => {}
                        Err(e) => {
                            println!("Error message: {}", e);
                        }
                    }
                    return Ok(Event::TargetStopped(SingleThreadStopReason::Terminated(
                        Signal::EXC_BAD_ACCESS,
                    )));
                }

                let exit_code = res.unwrap();
                return if exit_code.is_none() {
                    if target
                        .breakpoints
                        .contains(&target.simulator.borrow_mut().hart_state.pc)
                    {
                        Ok(Event::TargetStopped(SingleThreadStopReason::SwBreak(())))
                    } else {
                        Ok(Event::TargetStopped(SingleThreadStopReason::DoneStep))
                    }
                } else {
                    let exit_code = exit_code.unwrap();
                    match exit_code {
                        ExitCode::Paused(_) => {
                            Ok(Event::TargetStopped(SingleThreadStopReason::SwBreak(())))
                        }
                        ExitCode::Halted(reason) => Ok(Event::TargetStopped(
                            SingleThreadStopReason::Exited(reason as u8),
                        )),
                        ExitCode::HwWatchPoint((kind, addr)) => {
                            Ok(Event::TargetStopped(SingleThreadStopReason::Watch {
                                tid: (),
                                kind,
                                addr,
                            }))
                        }
                    }
                };
            }
            ExecMode::Continue => {
                let mut cycles = 0;
                loop {
                    if cycles % 1024 == 0 {
                        // poll for incoming data
                        if poll_incoming_data() {
                            let byte = conn
                                .read()
                                .map_err(run_blocking::WaitForStopReasonError::Connection)?;
                            return Ok(Event::IncomingData(byte));
                        }
                    }
                    cycles += 1;

                    let res = target.simulator.borrow_mut().step();
                    if res.is_err() {
                        match res {
                            Ok(_) => {}
                            Err(e) => {
                                println!("Error message: {}", e);
                            }
                        }
                        return Ok(Event::TargetStopped(SingleThreadStopReason::Terminated(
                            Signal::EXC_BAD_ACCESS,
                        )));
                    }

                    let exit_code = res.unwrap();
                    if exit_code.is_some() {
                        let exit_code = exit_code.unwrap();

                        return match exit_code {
                            ExitCode::Paused(_) => {
                                Ok(Event::TargetStopped(SingleThreadStopReason::SwBreak(())))
                            }
                            ExitCode::Halted(reason) => Ok(Event::TargetStopped(
                                SingleThreadStopReason::Exited(reason as u8),
                            )),
                            ExitCode::HwWatchPoint((kind, addr)) => {
                                Ok(Event::TargetStopped(SingleThreadStopReason::Watch {
                                    tid: (),
                                    kind,
                                    addr,
                                }))
                            }
                        };
                    } else {
                        if target
                            .breakpoints
                            .contains(&target.simulator.borrow_mut().hart_state.pc)
                        {
                            return Ok(Event::TargetStopped(SingleThreadStopReason::SwBreak(())));
                        }
                    }
                }
            }
            ExecMode::RangeStep(start, end) => {
                let mut cycles = 0;
                loop {
                    if cycles % 1024 == 0 {
                        if poll_incoming_data() {
                            let byte = conn
                                .read()
                                .map_err(run_blocking::WaitForStopReasonError::Connection)?;
                            return Ok(Event::IncomingData(byte));
                        }
                    }
                    cycles += 1;

                    let res = target.simulator.borrow_mut().step();
                    if res.is_err() {
                        match res {
                            Ok(_) => {}
                            Err(e) => {
                                println!("Error message: {}", e);
                            }
                        }
                        return Ok(Event::TargetStopped(SingleThreadStopReason::Terminated(
                            Signal::EXC_BAD_ACCESS,
                        )));
                    }

                    let exit_code = res.unwrap();
                    if exit_code.is_some() {
                        let exit_code = exit_code.unwrap();

                        return match exit_code {
                            ExitCode::Paused(_) => {
                                Ok(Event::TargetStopped(SingleThreadStopReason::SwBreak(())))
                            }
                            ExitCode::Halted(reason) => Ok(Event::TargetStopped(
                                SingleThreadStopReason::Exited(reason as u8),
                            )),
                            ExitCode::HwWatchPoint((kind, addr)) => {
                                Ok(Event::TargetStopped(SingleThreadStopReason::Watch {
                                    tid: (),
                                    kind,
                                    addr,
                                }))
                            }
                        };
                    }

                    if !(start..end).contains(&target.simulator.borrow_mut().hart_state.pc) {
                        return Ok(Event::TargetStopped(SingleThreadStopReason::DoneStep));
                    }

                    if target
                        .breakpoints
                        .contains(&target.simulator.borrow_mut().hart_state.pc)
                    {
                        return Ok(Event::TargetStopped(SingleThreadStopReason::SwBreak(())));
                    }
                }
            }
            ExecMode::Interrupted => {
                if poll_incoming_data() {
                    let byte = conn
                        .read()
                        .map_err(run_blocking::WaitForStopReasonError::Connection)?;
                    return Ok(Event::IncomingData(byte));
                }
                return Ok(Event::TargetStopped(SingleThreadStopReason::Signal(
                    Signal::SIGINT,
                )));
            }
        }
    }

    fn on_interrupt(
        target: &mut Self::Target,
    ) -> Result<Option<Self::StopReason>, <Self::Target as Target>::Error> {
        target.exec_mode = ExecMode::Interrupted;
        Ok(Some(SingleThreadStopReason::Signal(Signal::SIGINT)))
    }
}
