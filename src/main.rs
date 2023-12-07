extern crate alloc;
extern crate core;

use crate::serializer::to_vec;
use anyhow::anyhow;
use core::str::from_utf8;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read;
use std::rc::Rc;

pub mod vm;

pub mod debug;
mod serializer;

fn main() {
    let mem = Rc::new(RefCell::new(vm::memory::Memory::default()));
    let mut elf_data = Vec::<u8>::new();

    let mut fs = std::fs::File::open("code")
        .map_err(|err| anyhow!("cannot open the code file. {err}"))
        .unwrap();
    fs.read_to_end(&mut elf_data)
        .map_err(|err| anyhow!("cannot read the code file. {err}"))
        .unwrap();
    drop(fs);

    let entry = vm::loader::load_elf(mem.clone(), &elf_data).unwrap();

    let input = vec![0; 64];

    let simulator = Rc::new(RefCell::new(vm::simulator::Simulator::new(
        mem,
        entry,
        &HashMap::new(),
    )));
    simulator
        .borrow_mut()
        .write(
            crate::vm::fileno::STDIN,
            &bytemuck::cast_slice(&to_vec(&input).unwrap()),
        )
        .unwrap();

    debug::debugger_takeover(elf_data.clone(), simulator.clone()).unwrap();

    let mut stdout = Vec::<u8>::new();
    let mut stderr = Vec::<u8>::new();
    let mut journal = Vec::<u8>::new();

    simulator
        .borrow_mut()
        .read_to_end(vm::fileno::STDOUT, &mut stdout)
        .unwrap();
    simulator
        .borrow_mut()
        .read_to_end(vm::fileno::STDERR, &mut stderr)
        .unwrap();
    simulator
        .borrow_mut()
        .read_to_end(vm::fileno::JOURNAL, &mut journal)
        .unwrap();

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
}
