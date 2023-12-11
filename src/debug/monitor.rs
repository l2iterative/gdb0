use crate::debug::debugger::Debugger;
use crate::vm::session_cycle::*;
use gdbstub::outputln;
use gdbstub::target::ext::monitor_cmd::{ConsoleOutput, MonitorCmd};

impl MonitorCmd for Debugger {
    fn handle_monitor_cmd(
        &mut self,
        cmd: &[u8],
        mut out: ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        let cmd = match core::str::from_utf8(cmd) {
            Ok(cmd) => cmd,
            Err(_) => {
                outputln!(out, "command must be valid UTF-8");
                return Ok(());
            }
        };
        if cmd.starts_with('v') {
            let sim_ref = self.simulator.borrow();
            let count_ref = sim_ref.session_cycle_count.borrow();
            outputln!(out, "{} segments finished, current segment has taken {} cycles, {} pages are loaded, {} pages need to be stored", count_ref.num_segment,
                count_ref.cur_segment_cycle + PRE_CYCLE + POST_CYCLE + OTHER_CONST_CYCLE,
                count_ref.cur_segment_resident.len(), count_ref.cur_segment_dirty.len());
        } else if cmd.starts_with('c') {
            let sim_ref = self.simulator.borrow();
            let count_ref = sim_ref.session_cycle_count.borrow();
            outputln!(out, "{}", count_ref.get_session_cycle());
        } else {
            outputln!(out, "Supported commands: c(ycle) -- display cycle counts, v(erbose) -- display detailed cycle information");
        }

        Ok(())
    }
}
