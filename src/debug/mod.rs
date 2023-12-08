use crate::vm::simulator::Simulator;
use alloc::rc::Rc;
use std::cell::RefCell;
use std::collections::HashSet;
use std::net::{TcpListener, TcpStream};

use gdbstub::conn::ConnectionExt;
use gdbstub::stub::{DisconnectReason, GdbStub};

use crate::debug::debugger::{Debugger, ExecMode};
use crate::vm::ExitCode;
use anyhow::Result;

pub mod debugger;

/// Copy all bytes of `data` to `buf`.
/// Return the size of data copied.
pub fn copy_to_buf(data: &[u8], buf: &mut [u8]) -> usize {
    let len = buf.len().min(data.len());
    buf[..len].copy_from_slice(&data[..len]);
    len
}

/// Copy a range of `data` (start at `offset` with a size of `length`) to `buf`.
/// Return the size of data copied. Returns 0 if `offset >= buf.len()`.
///
/// Mainly used by qXfer:_object_:read commands.
pub fn copy_range_to_buf(data: &[u8], offset: u64, length: usize, buf: &mut [u8]) -> usize {
    let offset = offset as usize;
    if offset > data.len() {
        return 0;
    }

    let start = offset;
    let end = (offset + length).min(data.len());
    copy_to_buf(&data[start..end], buf)
}

fn wait_for_tcp(port: u16) -> Result<TcpStream> {
    let sockaddr = format!("127.0.0.1:{}", port);
    eprintln!("Waiting for a GDB connection on {:?}...", sockaddr);

    let sock = TcpListener::bind(sockaddr)?;
    let (stream, addr) = sock.accept()?;
    eprintln!("Debugger connected from {}", addr);

    Ok(stream)
}

pub fn debugger_takeover(elf: Vec<u8>, simulator: Rc<RefCell<Simulator>>) -> Result<()> {
    let connection: Box<dyn ConnectionExt<Error = std::io::Error>> = Box::new(wait_for_tcp(9000)?);
    let gdb = GdbStub::new(connection);

    let mut emu = Debugger {
        elf,
        simulator,
        exec_mode: ExecMode::Continue,
        breakpoints: HashSet::new(),
    };

    match gdb.run_blocking::<Debugger>(&mut emu) {
        Ok(disconnect_reason) => match disconnect_reason {
            DisconnectReason::Disconnect => {
                println!("GDB client has disconnected. Running to completion...");

                loop {
                    let res = emu.simulator.borrow_mut().step();
                    if res.is_err() {
                        match res {
                            Ok(_) => {}
                            Err(e) => {
                                println!("Error message: {}", e);
                            }
                        }
                        break;
                    }

                    let exit_code = res.unwrap();
                    match exit_code {
                        None => {}
                        Some(exit_code) => match exit_code {
                            ExitCode::Paused(code) => {
                                println!("Target paused with code {}!", code);
                                break;
                            }
                            ExitCode::Halted(code) => {
                                println!("Target exited with code {}!", code);
                                break;
                            }
                            ExitCode::HwWatchPoint(_) => {}
                        },
                    }
                }
            }
            DisconnectReason::TargetExited(code) => {
                println!("Target exited with code {}!", code)
            }
            DisconnectReason::TargetTerminated(sig) => {
                println!("Target terminated with signal {}!", sig)
            }
            DisconnectReason::Kill => println!("GDB sent a kill command!"),
        },
        Err(e) => {
            if e.is_target_error() {
                println!(
                    "target encountered a fatal error: {}",
                    e.into_target_error().unwrap()
                )
            } else if e.is_connection_error() {
                let (e, kind) = e.into_connection_error().unwrap();
                println!("connection error: {:?} - {}", kind, e,)
            } else {
                println!("gdbstub encountered a fatal error: {}", e)
            }
        }
    }

    Ok(())
}
