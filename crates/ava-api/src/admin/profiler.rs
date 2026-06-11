// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The performance profiler behind `admin.{startCPUProfiler,stopCPUProfiler,
//! memoryProfile,lockProfile}` (mirror Go `utils/profiler/profiler.go`;
//! specs 12 §3.5).
//!
//! File names and start/stop error strings are byte-exact with Go. The data
//! sources necessarily differ from the Go runtime's:
//!
//! - **CPU** — real: the `pprof` crate's sampling profiler (100 Hz, the Go
//!   `runtime/pprof` default), written to `<profile-dir>/cpu.profile` in pprof
//!   protobuf format (`go tool pprof`-compatible) on stop. Divergence from Go:
//!   `cpu.profile` is created on start but only gets its contents on stop
//!   (samples are buffered in memory, not streamed).
//! - **Memory** (`mem.profile`) and **lock** (`lock.profile`) — **unsupported**:
//!   Rust has no stable equivalent of the Go runtime's heap / mutex profiles
//!   (no allocator hooks without swapping the global allocator; no
//!   `runtime.SetMutexProfileFraction`). Rather than write fabricated data we
//!   return a clean error naming the limitation; the API surface (method set,
//!   args/reply shapes) stays Go-parity.

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
// pprof's protos are generated against its own pinned prost (0.12, vs the
// workspace's 0.13); using its `Message` re-export keeps the trait and the
// generated types on the same prost version.
use pprof::protos::Message;

/// Name of the file the CPU profile is written to (Go `cpuProfileFile`).
pub const CPU_PROFILE_FILE: &str = "cpu.profile";
/// Name of the file a memory profile would be written to (Go `memProfileFile`).
pub const MEM_PROFILE_FILE: &str = "mem.profile";
/// Name of the file a lock profile would be written to (Go `lockProfileFile`).
pub const LOCK_PROFILE_FILE: &str = "lock.profile";

/// The CPU sampling frequency (Hz). Go's `runtime/pprof` samples at 100 Hz.
const CPU_HZ: i32 = 100;

/// Errors from the profiler. `AlreadyRunning` / `NotRunning` are byte-exact
/// with Go (`errCPUProfilerRunning` / `errCPUProfilerNotRunning`).
#[derive(Debug, thiserror::Error)]
pub enum ProfilerError {
    /// `startCPUProfiler` while a CPU profile is being taken.
    #[error("cpu profiler already running")]
    AlreadyRunning,
    /// `stopCPUProfiler` with no CPU profile in progress.
    #[error("cpu profiler doesn't exist")]
    NotRunning,
    /// `memoryProfile`: no Go-heap-profile equivalent exists in Rust (see
    /// module docs).
    #[error("memory profiling is not supported by this node implementation")]
    MemoryUnsupported,
    /// `lockProfile`: no Go-mutex-profile equivalent exists in Rust (see
    /// module docs).
    #[error("lock profiling is not supported by this node implementation")]
    LockUnsupported,
    /// Creating the profile directory/file or writing the profile failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The sampling profiler failed to start or to build its report.
    #[error("cpu profiler error: {0}")]
    Cpu(String),
}

/// The in-flight CPU profile: the sampling guard (dropping it stops sampling).
type CpuGuard = pprof::ProfilerGuard<'static>;

/// Provides helper methods for measuring the current performance of this
/// process (mirror Go `profiler.profiler`).
pub struct Profiler {
    cpu_profile_name: PathBuf,
    dir: PathBuf,
    cpu: Mutex<Option<CpuGuard>>,
}

impl Profiler {
    /// A profiler writing into `dir` (Go `profiler.New`).
    #[must_use]
    pub fn new(dir: &Path) -> Self {
        Self {
            cpu_profile_name: dir.join(CPU_PROFILE_FILE),
            dir: dir.to_path_buf(),
            cpu: Mutex::new(None),
        }
    }

    /// Starts measuring the cpu utilization of this process (Go
    /// `StartCPUProfiler`).
    ///
    /// # Errors
    /// [`ProfilerError::AlreadyRunning`] if a profile is already in progress;
    /// I/O errors creating `profile-dir`/`cpu.profile`; profiler start failure.
    pub fn start_cpu_profiler(&self) -> Result<(), ProfilerError> {
        let mut cpu = self.cpu.lock();
        if cpu.is_some() {
            return Err(ProfilerError::AlreadyRunning);
        }

        std::fs::create_dir_all(&self.dir)?;
        // Go opens the output file at start; mirror that (it also surfaces
        // permission problems immediately rather than at stop).
        std::fs::File::create(&self.cpu_profile_name)?;

        let guard = pprof::ProfilerGuardBuilder::default()
            .frequency(CPU_HZ)
            .build()
            .map_err(|e| ProfilerError::Cpu(e.to_string()))?;
        *cpu = Some(guard);
        Ok(())
    }

    /// Stops the CPU profile and writes it to `cpu.profile` (Go
    /// `StopCPUProfiler`).
    ///
    /// # Errors
    /// [`ProfilerError::NotRunning`] if no profile is in progress; report /
    /// write failures.
    pub fn stop_cpu_profiler(&self) -> Result<(), ProfilerError> {
        let mut cpu = self.cpu.lock();
        let guard = cpu.take().ok_or(ProfilerError::NotRunning)?;

        let report = guard
            .report()
            .build()
            .map_err(|e| ProfilerError::Cpu(e.to_string()))?;
        let profile = report
            .pprof()
            .map_err(|e| ProfilerError::Cpu(e.to_string()))?;
        drop(guard); // stop sampling before the file write
        std::fs::write(&self.cpu_profile_name, profile.encode_to_vec())?;
        Ok(())
    }

    /// Dumps the current memory utilization of this process (Go
    /// `MemoryProfile`).
    ///
    /// # Errors
    /// Always [`ProfilerError::MemoryUnsupported`] — see module docs.
    pub fn memory_profile(&self) -> Result<(), ProfilerError> {
        Err(ProfilerError::MemoryUnsupported)
    }

    /// Dumps the current lock statistics of this process (Go `LockProfile`).
    ///
    /// # Errors
    /// Always [`ProfilerError::LockUnsupported`] — see module docs.
    pub fn lock_profile(&self) -> Result<(), ProfilerError> {
        Err(ProfilerError::LockUnsupported)
    }
}
