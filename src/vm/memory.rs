use crate::vm::session_cycle::SessionCycleCount;
use alloc::rc::Rc;
use gdbstub::target::ext::breakpoints::WatchKind;
use rrs_lib::MemAccessSize;
use std::cell::RefCell;
use std::collections::BTreeMap;

pub const GUEST_MIN_MEM: usize = 0x0000_0400;
pub const GUEST_MAX_MEM: usize = 0x0C00_0000;

#[derive(Default)]
pub struct Memory {
    pub map: BTreeMap<u32, [u32; 256]>,
    pub hw_watchpoints: Vec<(u32, u32, WatchKind)>,
    pub watch_trigger: Option<(WatchKind, u32)>,
    pub session_cycle_callback: Option<Rc<RefCell<SessionCycleCount>>>,
}

impl Memory {
    pub fn with_session_cycle_callback(&mut self, callback: Rc<RefCell<SessionCycleCount>>) {
        self.session_cycle_callback = Some(callback);
    }

    fn check_watchpoints(&mut self, addr: u32, len: u32, is_write: bool) {
        if self.watch_trigger.is_some() {
            return;
        }
        for entry in self.hw_watchpoints.iter() {
            if is_write && entry.2 == WatchKind::Read {
                continue;
            }
            if !is_write && entry.2 == WatchKind::Write {
                continue;
            }

            let watch_start = entry.0;
            let watch_end = watch_start + entry.1;

            let action_start = addr;
            let action_end = addr + len;

            if action_start < watch_start && action_end >= watch_start {
                self.watch_trigger = Some((entry.2, addr));
                return;
            } else if action_start >= watch_start && action_start < watch_end {
                self.watch_trigger = Some((entry.2, addr));
                return;
            }
        }
    }

    pub(crate) fn read_mem_with_privileges(
        &mut self,
        addr: u32,
        size: MemAccessSize,
        privileged: bool,
    ) -> Option<u32> {
        if ((addr as usize) < GUEST_MIN_MEM || (addr as usize) > GUEST_MAX_MEM) {
            return None;
        }

        let page_idx = addr >> 10;
        if !self.map.contains_key(&page_idx) {
            self.map.insert(page_idx, [0u32; 256]);
        }

        if !privileged && self.session_cycle_callback.is_some() {
            self.session_cycle_callback
                .as_ref()
                .unwrap()
                .borrow_mut()
                .callback_read_mem(page_idx);
        }

        let page_offset = (addr & 0x3ff) as usize;

        return match size {
            MemAccessSize::Byte => {
                if !privileged {
                    self.check_watchpoints(addr, 1, false);
                }
                let word = self.map.get(&page_idx).unwrap()[page_offset / 4];

                if page_offset % 4 == 0 {
                    Some(word & 0xff)
                } else if page_offset % 4 == 1 {
                    Some((word >> 8) & 0xff)
                } else if page_offset % 4 == 2 {
                    Some((word >> 16) & 0xff)
                } else {
                    Some((word >> 24) & 0xff)
                }
            }
            MemAccessSize::HalfWord => {
                if !privileged {
                    self.check_watchpoints(addr, 2, false);
                }
                let word = self.map.get(&page_idx).unwrap()[page_offset / 4];

                if page_offset % 4 == 2 {
                    Some((word >> 16) & 0xffff)
                } else {
                    Some(word & 0xffff)
                }
            }
            MemAccessSize::Word => {
                if !privileged {
                    self.check_watchpoints(addr, 4, false);
                }
                Some(self.map.get(&page_idx).unwrap()[page_offset / 4])
            }
        };
    }

    pub(crate) fn write_mem_with_privileges(
        &mut self,
        addr: u32,
        size: MemAccessSize,
        store_data: u32,
        privileged: bool,
    ) -> bool {
        if ((addr as usize) < GUEST_MIN_MEM || (addr as usize) > GUEST_MAX_MEM) {
            return false;
        }

        let page_idx = addr >> 10;
        if !self.map.contains_key(&page_idx) {
            self.map.insert(page_idx, [0u32; 256]);
        }

        if !privileged && self.session_cycle_callback.is_some() {
            self.session_cycle_callback
                .as_ref()
                .unwrap()
                .borrow_mut()
                .callback_write_mem(page_idx);
        }

        let page_offset = (addr & 0x3ff) as usize;

        match size {
            MemAccessSize::Byte => {
                if !privileged {
                    self.check_watchpoints(addr, 1, true);
                }
                let word = self.map.get(&page_idx).unwrap()[page_offset / 4];

                let new_word = if page_offset % 4 == 0 {
                    (word & 0xffffff00) | (store_data & 0xff)
                } else if page_offset % 4 == 1 {
                    (word & 0xffff00ff) | ((store_data & 0xff) << 8)
                } else if page_offset % 4 == 2 {
                    (word & 0xff00ffff) | ((store_data & 0xff) << 16)
                } else {
                    (word & 0x00ffffff) | ((store_data & 0xff) << 24)
                };

                self.map.get_mut(&page_idx).unwrap()[page_offset / 4] = new_word;
            }
            MemAccessSize::HalfWord => {
                if !privileged {
                    self.check_watchpoints(addr, 2, true);
                }
                let word = self.map.get(&page_idx).unwrap()[page_offset / 4];

                let new_word = if page_offset % 4 == 2 {
                    (word & 0x0000ffff) | ((store_data & 0xffff) << 16)
                } else {
                    (word & 0xffff0000) | (store_data & 0xffff)
                };

                self.map.get_mut(&page_idx).unwrap()[page_offset / 4] = new_word;
            }
            MemAccessSize::Word => {
                if !privileged {
                    self.check_watchpoints(addr, 4, true);
                }
                self.map.get_mut(&page_idx).unwrap()[page_offset / 4] = store_data;
            }
        }

        true
    }
}

impl rrs_lib::Memory for Memory {
    fn read_mem(&mut self, addr: u32, size: MemAccessSize) -> Option<u32> {
        self.read_mem_with_privileges(addr, size, false)
    }

    fn write_mem(&mut self, addr: u32, size: MemAccessSize, store_data: u32) -> bool {
        self.write_mem_with_privileges(addr, size, store_data, false)
    }
}
