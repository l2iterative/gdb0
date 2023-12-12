// The way that RISC Zero works is as follows.
//
// The ZK proves a very simple circuit that consists of the following rows.
// - pre loading, which sets up some lookup tables, loads some initial data into the RAM
// - body, a very simple row that just tells the VM to move forward
// - post loading, which does some cleanup work
//
// And the rest is the data.

use anyhow::{bail, Result};
use std::collections::HashSet;

// 1 cycle for triggering the byte init column
// 1561 cycles for triggering the byte setup column (32 * 1024 / 21 ceiling div = 1561)
// 1 cycle for triggering the RAM init column
// (64 + 8 + 8) elements to be put into the initial RAM data into the system, incurring 27 cycles
// 2 cycles for sending a RESET command
pub const PRE_CYCLE: usize = 1 + 1561 + 1 + 27 + 2;

// 2 cycles for the first RESET command
// 2 cycles for the second RESET command
// 1 cycle to end the byte column
// 1 cycle to end the RAM column
pub const POST_CYCLE: usize = 2 + 2 + 2;

// 73 cycles for the SHA_CYCLES
// 50 cycles for the ZK related work
pub const OTHER_CONST_CYCLE: usize = 73 + 50;

#[derive(Default)]
pub struct SessionCycleCount {
    pub num_segment: usize,

    pub cur_segment_cycle: usize,
    pub cur_segment_resident: HashSet<u32>,
    pub cur_segment_dirty: HashSet<u32>,

    pub cur_step_read: HashSet<u32>,
    pub cur_step_write: HashSet<u32>,
}

impl SessionCycleCount {
    fn update_cur_segment_total_cycle(&mut self, new_step_cycle: usize) -> bool {
        let new_segment_total_cycle =
            PRE_CYCLE + POST_CYCLE + OTHER_CONST_CYCLE + self.cur_segment_cycle + new_step_cycle;

        return if new_segment_total_cycle > 1048576 {
            // a new segment needs to be created
            self.num_segment += 1;
            self.cur_segment_cycle = 0;
            self.cur_segment_resident.clear();
            self.cur_segment_dirty.clear();

            true
        } else {
            self.cur_segment_cycle += new_step_cycle;

            false
        };
    }

    pub fn get_session_cycle(&self) -> usize {
        let segment_total_cycle =
            PRE_CYCLE + POST_CYCLE + OTHER_CONST_CYCLE + self.cur_segment_cycle;

        return self.num_segment * 1048576 + segment_total_cycle;
    }

    pub fn callback_read_mem(&mut self, page_idx: u32) {
        self.cur_step_read.insert(page_idx);
    }

    pub fn callback_write_mem(&mut self, page_idx: u32) {
        self.cur_step_write.insert(page_idx);
    }

    pub fn callback_step(&mut self, opcode_cycle: usize, extra_cycle: usize) {
        loop {
            let mut cur_step_page_read_cycle = 0;
            let mut new_segment_resident = Vec::new();

            for page_idx in self.cur_step_read.iter() {
                let mut cur_page_idx = *page_idx;
                loop {
                    if self.cur_segment_resident.contains(&cur_page_idx)
                        || new_segment_resident.contains(&cur_page_idx)
                    {
                        break;
                    }

                    // 219862 is the root's page ID.
                    if cur_page_idx == 219862 {
                        // The root page is shorter, and it only contains 22 u32, which means 11 blocks.
                        // based on 1 + SHA_INIT + (SHA_LOAD + SHA_MAIN) * blocks_per_page
                        cur_step_page_read_cycle += 1 + 5 + (16 + 52) * 11;
                        new_segment_resident.push(cur_page_idx);
                        break;
                    } else {
                        // Each other page has 16 blocks, making up 1024 bytes.
                        cur_step_page_read_cycle += 1 + 5 + (16 + 52) * 16;
                        new_segment_resident.push(cur_page_idx);
                    }

                    cur_page_idx = (0x0D00_0000 + cur_page_idx * 32) >> 10;
                }
            }

            let mut cur_step_page_write_cycle = 0;
            let mut new_segment_dirty = Vec::new();

            for page_idx in self.cur_step_write.iter() {
                let mut cur_page_idx = *page_idx;
                loop {
                    if self.cur_segment_dirty.contains(&cur_page_idx)
                        || new_segment_dirty.contains(&cur_page_idx)
                    {
                        break;
                    }

                    // 219862 is the root's page ID.
                    if cur_page_idx == 219862 {
                        // The root page is shorter, and it only contains 22 u32, which means 11 blocks.
                        // based on 1 + SHA_INIT + (SHA_LOAD + SHA_MAIN) * blocks_per_page
                        cur_step_page_write_cycle += 1 + 5 + (16 + 52) * 11;
                        new_segment_dirty.push(cur_page_idx);
                        break;
                    } else {
                        // Each other page has 16 blocks, making up 1024 bytes.
                        cur_step_page_write_cycle += 1 + 5 + (16 + 52) * 16;
                        new_segment_dirty.push(cur_page_idx);
                    }

                    cur_page_idx = (0x0D00_0000 + cur_page_idx * 32) >> 10;
                }
            }

            let cur_step_total_cycle =
                opcode_cycle + extra_cycle + cur_step_page_read_cycle + cur_step_page_write_cycle;

            let redo = self.update_cur_segment_total_cycle(cur_step_total_cycle);
            if redo == false {
                for i in new_segment_resident {
                    self.cur_segment_resident.insert(i);
                }
                for i in new_segment_dirty {
                    self.cur_segment_dirty.insert(i);
                }

                self.cur_step_read.clear();
                self.cur_step_write.clear();

                return;
            }
        }
    }
}

pub fn get_opcode_cycle(insn: u32) -> Result<usize> {
    let opcode = insn & 0x0000007f;
    let funct3 = (insn & 0x00007000) >> 12;
    let funct7 = (insn & 0xfe000000) >> 25;

    Ok(match opcode {
        0b0000011 => 1,
        0b0010011 => match funct3 {
            0x0 | 0x1 | 0x2 | 0x3 => 1,
            0x4 | 0x5 | 0x6 | 0x7 => 2,
            _ => bail!("Illegal instruction1"),
        },
        0b0010111 => 1,
        0b0100011 => 1,
        0b0110011 => match (funct3, funct7) {
            (0x0, 0x00) => 1,
            (0x0, 0x20) => 1,
            (0x1, 0x00) => 1,
            (0x2, 0x00) => 1,
            (0x3, 0x00) => 1,
            (0x4, 0x00) => 2,
            (0x5, 0x00) => 2,
            (0x5, 0x20) => 2,
            (0x6, 0x00) => 2,
            (0x7, 0x00) => 2,
            (0x0, 0x01) => 1,
            (0x1, 0x01) => 1,
            (0x2, 0x01) => 1,
            (0x3, 0x01) => 1,
            (0x4, 0x01) => 2,
            (0x5, 0x01) => 2,
            (0x6, 0x01) => 2,
            (0x7, 0x01) => 2,
            _ => bail!("Illegal instruction2"),
        },
        0b0110111 => 1,
        0b1100011 => 1,
        0b1100111 => 1,
        0b1101111 => 1,
        0b1110011 => 1,
        _ => bail!("Illegal instruction3"),
    })
}
