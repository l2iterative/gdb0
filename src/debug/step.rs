use crate::debug::debugger::{Debugger, ExecMode};
use gdbstub::arch::Arch;
use gdbstub::common::Signal;
use gdbstub::target::ext::base::reverse_exec::{ReverseContOps, ReverseStepOps};
use gdbstub::target::ext::base::singlethread::{
    SingleThreadRangeStepping, SingleThreadRangeSteppingOps, SingleThreadResume,
    SingleThreadSingleStep, SingleThreadSingleStepOps,
};

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
