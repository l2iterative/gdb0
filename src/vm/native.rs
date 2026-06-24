use anyhow::{anyhow, Context, Result};
use risc0_binfmt::ProgramBinary;
use risc0_zkos_v1compat::V1COMPAT_ELF;
use risc0_zkvm::{ExecutorEnv, ExecutorImpl, SimpleSegmentRef};
use std::thread;

pub const DEFAULT_NATIVE_SESSION_LIMIT: u64 = 1 << 20;
pub const NATIVE_THREAD_STACK_SIZE: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct NativeProgramInfo {
    pub wrapped_len: usize,
    pub image_id: String,
    pub kernel_id: String,
}

#[derive(Debug)]
pub enum NativeSmokeStatus {
    Completed {
        exit_code: String,
        segments: usize,
        journal_bytes: usize,
        user_cycles: u64,
        total_cycles: u64,
    },
    ExecutionError(String),
}

#[derive(Debug)]
pub struct NativeSmokeReport {
    pub program: NativeProgramInfo,
    pub status: NativeSmokeStatus,
}

pub fn encode_v1compat_binary(user_elf: &[u8]) -> Vec<u8> {
    ProgramBinary::new(user_elf, V1COMPAT_ELF).encode()
}

pub fn inspect_v1compat_binary(user_elf: &[u8]) -> Result<NativeProgramInfo> {
    let wrapped = encode_v1compat_binary(user_elf);
    let binary = ProgramBinary::decode(&wrapped).context("decode wrapped ProgramBinary")?;

    Ok(NativeProgramInfo {
        wrapped_len: wrapped.len(),
        image_id: format!("{:?}", binary.compute_image_id()?),
        kernel_id: format!("{:?}", binary.kernel_id()?),
    })
}

pub fn run_native_smoke(
    user_elf: &[u8],
    input_words: &[u32],
    session_limit: u64,
    segment_limit_po2: Option<u32>,
) -> Result<NativeSmokeReport> {
    let wrapped = encode_v1compat_binary(user_elf);
    let program = inspect_v1compat_binary(user_elf)?;

    let mut env_builder = ExecutorEnv::builder();
    env_builder
        .write_slice(input_words)
        .session_limit(Some(session_limit));

    if let Some(limit) = segment_limit_po2 {
        env_builder.segment_limit_po2(limit);
    }

    let env = env_builder.build().context("build native ExecutorEnv")?;
    let mut executor =
        ExecutorImpl::from_elf(env, &wrapped).context("construct native RISC Zero executor")?;

    let status =
        match executor.run_with_callback(|segment| Ok(Box::new(SimpleSegmentRef::new(segment)))) {
            Ok(session) => NativeSmokeStatus::Completed {
                exit_code: format!("{:?}", session.exit_code),
                segments: session.segments.len(),
                journal_bytes: session
                    .journal
                    .as_ref()
                    .map(|journal| journal.bytes.len())
                    .unwrap_or(0),
                user_cycles: session.user_cycles,
                total_cycles: session.total_cycles,
            },
            Err(err) => NativeSmokeStatus::ExecutionError(format!("{err:#}")),
        };

    Ok(NativeSmokeReport { program, status })
}

pub fn run_native_smoke_threaded(
    user_elf: Vec<u8>,
    input_words: Vec<u32>,
    session_limit: u64,
    segment_limit_po2: Option<u32>,
) -> Result<NativeSmokeReport> {
    thread::Builder::new()
        .name("risc0-native-smoke".to_string())
        .stack_size(NATIVE_THREAD_STACK_SIZE)
        .spawn(move || run_native_smoke(&user_elf, &input_words, session_limit, segment_limit_po2))
        .context("spawn native RISC Zero executor thread")?
        .join()
        .map_err(|_| anyhow!("native RISC Zero executor thread panicked"))?
}

pub fn run_native_gdb(
    user_elf: &[u8],
    input_words: &[u32],
    segment_limit_po2: Option<u32>,
) -> Result<()> {
    let wrapped = encode_v1compat_binary(user_elf);
    let mut env_builder = ExecutorEnv::builder();
    env_builder.write_slice(input_words);

    if let Some(limit) = segment_limit_po2 {
        env_builder.segment_limit_po2(limit);
    }

    let env = env_builder.build().context("build native ExecutorEnv")?;
    let mut executor =
        ExecutorImpl::from_elf(env, &wrapped).context("construct native RISC Zero executor")?;
    executor.run_with_debugger()
}

pub fn run_native_gdb_threaded(
    user_elf: Vec<u8>,
    input_words: Vec<u32>,
    segment_limit_po2: Option<u32>,
) -> Result<()> {
    thread::Builder::new()
        .name("risc0-native-gdb".to_string())
        .stack_size(NATIVE_THREAD_STACK_SIZE)
        .spawn(move || run_native_gdb(&user_elf, &input_words, segment_limit_po2))
        .context("spawn native RISC Zero debugger thread")?
        .join()
        .map_err(|_| anyhow!("native RISC Zero debugger thread panicked"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE_ELF: &[u8] = include_bytes!("../../code");

    #[test]
    fn raw_code_wraps_as_v1compat_program_binary() {
        let wrapped = encode_v1compat_binary(CODE_ELF);
        let binary = ProgramBinary::decode(&wrapped).unwrap();

        assert_eq!(binary.user_elf, CODE_ELF);
        assert_eq!(binary.kernel_elf, V1COMPAT_ELF);
        binary.to_image().unwrap();
        binary.compute_image_id().unwrap();
    }
}
