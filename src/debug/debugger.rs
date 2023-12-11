use crate::vm::simulator::Simulator;
use crate::vm::ExitCode;
use alloc::rc::Rc;
use gdbstub::common::Signal;
use gdbstub::conn::{Connection, ConnectionExt};
use gdbstub::stub::run_blocking::{Event, WaitForStopReasonError};
use gdbstub::stub::{run_blocking, SingleThreadStopReason};
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::breakpoints::BreakpointsOps;
use gdbstub::target::ext::exec_file::ExecFileOps;
use gdbstub::target::ext::host_io::HostIoOps;
use gdbstub::target::ext::monitor_cmd::MonitorCmdOps;
use gdbstub::target::Target;
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

    fn support_monitor_cmd(&mut self) -> Option<MonitorCmdOps<'_, Self>> {
        Some(self)
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
