extern crate alloc;
extern crate core;

use crate::serializer::to_vec;
use anyhow::{anyhow, bail, Context, Result};
use core::str::from_utf8;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::rc::Rc;

pub mod vm;

pub mod debug;
mod serializer;

enum Mode {
    StandaloneGdb,
    NativeSmoke,
    NativeGdb,
}

struct Args {
    mode: Mode,
    code_path: PathBuf,
    session_limit: u64,
    segment_limit_po2: Option<u32>,
}

fn usage() -> &'static str {
    "Usage: r0db [--code PATH] [--native-smoke|--native-gdb] [--session-limit CYCLES] [--segment-limit-po2 PO2]\n\
\n\
Modes:\n\
  default          Run the standalone gdbstub debugger on 127.0.0.1:9000\n\
  --native-smoke  Wrap the raw ELF with RISC Zero v1compat and run a bounded native ExecutorImpl smoke\n\
  --native-gdb    Run RISC Zero's native ExecutorImpl debugger on its chosen local port"
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        mode: Mode::StandaloneGdb,
        code_path: PathBuf::from("code"),
        session_limit: vm::native::DEFAULT_NATIVE_SESSION_LIMIT,
        segment_limit_po2: None,
    };

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--native-smoke" => args.mode = Mode::NativeSmoke,
            "--native-gdb" => args.mode = Mode::NativeGdb,
            "--code" => {
                args.code_path = iter
                    .next()
                    .map(PathBuf::from)
                    .context("--code requires a path")?;
            }
            "--session-limit" => {
                args.session_limit = iter
                    .next()
                    .context("--session-limit requires a value")?
                    .parse()
                    .context("invalid --session-limit")?;
            }
            "--segment-limit-po2" => {
                args.segment_limit_po2 = Some(
                    iter.next()
                        .context("--segment-limit-po2 requires a value")?
                        .parse()
                        .context("invalid --segment-limit-po2")?,
                );
            }
            other => bail!("unknown argument {other:?}\n{}", usage()),
        }
    }

    Ok(args)
}

fn read_elf(path: &PathBuf) -> Result<Vec<u8>> {
    let mut elf_data = Vec::<u8>::new();
    let mut fs = std::fs::File::open(path)
        .map_err(|err| anyhow!("cannot open the code file {}. {err}", path.display()))?;
    fs.read_to_end(&mut elf_data)
        .map_err(|err| anyhow!("cannot read the code file {}. {err}", path.display()))?;
    Ok(elf_data)
}

fn sample_input_words() -> Result<Vec<u32>> {
    let input = vec![0; 64];
    to_vec(&input).map_err(Into::into)
}

fn run_standalone_gdb(elf_data: Vec<u8>, input_words: &[u32]) -> Result<()> {
    let mem = Rc::new(RefCell::new(vm::memory::Memory::default()));
    let entry = vm::loader::load_elf(mem.clone(), &elf_data)?;

    let simulator = Rc::new(RefCell::new(vm::simulator::Simulator::new(
        mem,
        entry,
        &HashMap::new(),
    )));
    simulator
        .borrow_mut()
        .write(crate::vm::fileno::STDIN, bytemuck::cast_slice(input_words))?;

    debug::debugger_takeover(elf_data.clone(), simulator.clone())?;

    let mut stdout = Vec::<u8>::new();
    let mut stderr = Vec::<u8>::new();
    let mut journal = Vec::<u8>::new();

    simulator
        .borrow_mut()
        .read_to_end(vm::fileno::STDOUT, &mut stdout)?;
    simulator
        .borrow_mut()
        .read_to_end(vm::fileno::STDERR, &mut stderr)?;
    simulator
        .borrow_mut()
        .read_to_end(vm::fileno::JOURNAL, &mut journal)?;

    if simulator.borrow().stdout.get_ref().len() != 0 {
        println!(
            "stdout: {} bytes",
            simulator.borrow().stdout.get_ref().len()
        );
        println!("{}", from_utf8(&stdout).unwrap());
    }

    if simulator.borrow().stderr.get_ref().len() != 0 {
        println!(
            "stderr: {} bytes",
            simulator.borrow().stderr.get_ref().len()
        );
        println!("{}", from_utf8(&stderr).unwrap());
    }

    if simulator.borrow().journal.get_ref().len() != 0 {
        println!(
            "journal: {} bytes",
            simulator.borrow().journal.get_ref().len()
        );
        println!("{}", from_utf8(&journal).unwrap());
    }

    Ok(())
}

fn print_native_smoke(report: vm::native::NativeSmokeReport) {
    println!("native executor: constructed");
    println!(
        "wrapped ProgramBinary bytes: {}",
        report.program.wrapped_len
    );
    println!("image_id: {}", report.program.image_id);
    println!("kernel_id: {}", report.program.kernel_id);

    match report.status {
        vm::native::NativeSmokeStatus::Completed {
            exit_code,
            segments,
            journal_bytes,
            user_cycles,
            total_cycles,
        } => {
            println!("native smoke status: completed");
            println!("exit_code: {exit_code}");
            println!("segments: {segments}");
            println!("journal bytes: {journal_bytes}");
            println!("user cycles: {user_cycles}");
            println!("total cycles: {total_cycles}");
        }
        vm::native::NativeSmokeStatus::ExecutionError(err) => {
            println!("native smoke status: execution-error");
            println!("{err}");
        }
    }
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let elf_data = read_elf(&args.code_path)?;
    let input_words = sample_input_words()?;

    match args.mode {
        Mode::StandaloneGdb => run_standalone_gdb(elf_data, &input_words),
        Mode::NativeSmoke => {
            let report = vm::native::run_native_smoke_threaded(
                elf_data,
                input_words,
                args.session_limit,
                args.segment_limit_po2,
            )?;
            print_native_smoke(report);
            Ok(())
        }
        Mode::NativeGdb => {
            vm::native::run_native_gdb_threaded(elf_data, input_words, args.segment_limit_po2)
        }
    }
}
