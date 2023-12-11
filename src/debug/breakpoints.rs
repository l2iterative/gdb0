use crate::debug::debugger::Debugger;
use gdbstub::arch::Arch;
use gdbstub::target::ext::breakpoints::{
    Breakpoints, HwWatchpoint, HwWatchpointOps, SwBreakpoint, SwBreakpointOps, WatchKind,
};
use gdbstub::target::TargetResult;

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
