use gdbstub::common::Pid;
use gdbstub::target::ext::exec_file::ExecFile;
use gdbstub::target::ext::host_io::{FsKind, HostIo, HostIoClose, HostIoCloseOps, HostIoErrno, HostIoError, HostIoFstat, HostIoFstatOps, HostIoOpen, HostIoOpenFlags, HostIoOpenMode, HostIoOpenOps, HostIoPread, HostIoPreadOps, HostIoReadlink, HostIoReadlinkOps, HostIoResult, HostIoSetfs, HostIoSetfsOps, HostIoStat};
use gdbstub::target::TargetResult;
use crate::debug::debugger::Debugger;

impl ExecFile for Debugger {
    fn get_exec_file(
        &self,
        _pid: Option<Pid>,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let filename = b"/r0code.elf";
        Ok(crate::debug::copy_range_to_buf(
            filename, offset, length, buf,
        ))
    }
}

impl HostIo for Debugger {
    fn support_open(&mut self) -> Option<HostIoOpenOps<'_, Self>> {
        Some(self)
    }

    fn support_close(&mut self) -> Option<HostIoCloseOps<'_, Self>> {
        Some(self)
    }

    fn support_pread(&mut self) -> Option<HostIoPreadOps<'_, Self>> {
        Some(self)
    }

    fn support_fstat(&mut self) -> Option<HostIoFstatOps<'_, Self>> {
        Some(self)
    }

    fn support_readlink(&mut self) -> Option<HostIoReadlinkOps<'_, Self>> {
        Some(self)
    }

    fn support_setfs(&mut self) -> Option<HostIoSetfsOps<'_, Self>> {
        Some(self)
    }
}

impl HostIoOpen for Debugger {
    fn open(
        &mut self,
        filename: &[u8],
        _flags: HostIoOpenFlags,
        _mode: HostIoOpenMode,
    ) -> HostIoResult<u32, Self> {
        if filename == b"/r0code.elf" {
            return Ok(0);
        }
        return Err(HostIoError::Errno(HostIoErrno::ENOENT));
    }
}

impl HostIoClose for Debugger {
    fn close(&mut self, _fd: u32) -> HostIoResult<(), Self> {
        return Ok(());
    }
}

impl HostIoPread for Debugger {
    fn pread(
        &mut self,
        fd: u32,
        count: usize,
        offset: u64,
        buf: &mut [u8],
    ) -> HostIoResult<usize, Self> {
        return if fd == 0 {
            Ok(crate::debug::copy_range_to_buf(
                &self.elf, offset, count, buf,
            ))
        } else {
            Err(HostIoError::Errno(HostIoErrno::EBADF))
        };
    }
}

impl HostIoFstat for Debugger {
    fn fstat(&mut self, fd: u32) -> HostIoResult<HostIoStat, Self> {
        if fd == 0 {
            return Ok(HostIoStat {
                st_dev: 0,
                st_ino: 0,
                st_mode: HostIoOpenMode::empty(),
                st_nlink: 0,
                st_uid: 0,
                st_gid: 0,
                st_rdev: 0,
                st_size: self.elf.len() as u64,
                st_blksize: 0,
                st_blocks: 0,
                st_atime: 0,
                st_mtime: 0,
                st_ctime: 0,
            });
        } else {
            return Err(HostIoError::Errno(HostIoErrno::EBADF));
        }
    }
}

impl HostIoSetfs for Debugger {
    fn setfs(&mut self, _fs: FsKind) -> HostIoResult<(), Self> {
        Ok(())
    }
}

impl HostIoReadlink for Debugger {
    fn readlink(&mut self, filename: &[u8], buf: &mut [u8]) -> HostIoResult<usize, Self> {
        return if filename == b"/proc/1/exe" {
            // Support `info proc exe` command
            let exe = b"/r0code.elf";
            Ok(crate::debug::copy_to_buf(exe, buf))
        } else if filename == b"/proc/1/cwd" {
            // Support `info proc cwd` command
            let cwd = b"/";
            Ok(crate::debug::copy_to_buf(cwd, buf))
        } else {
            Err(HostIoError::Errno(HostIoErrno::ENOENT))
        };
    }
}