use gdbstub::target::ext::monitor_cmd::{ConsoleOutput, MonitorCmd};
use crate::debug::debugger::Debugger;

impl MonitorCmd for Debugger {
    fn handle_monitor_cmd(&mut self, cmd: &[u8], out: ConsoleOutput<'_>) -> Result<(), Self::Error> {
        todo!()
    }
}