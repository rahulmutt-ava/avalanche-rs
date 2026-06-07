// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Spawning a plugin **subprocess** for the reverse-dial handshake
//! (`vms/rpcchainvm/runtime/subprocess`, specs 07 §5.1, §7.6).
//!
//! The host sets [`ENGINE_ADDRESS_KEY`](crate::ENGINE_ADDRESS_KEY) to the runtime
//! server address `R` and spawns the plugin binary. On **Linux** the child is
//! given `PR_SET_PDEATHSIG = SIGTERM` (the parent-death signal) via an isolated
//! `unsafe` `pre_exec` closure so the plugin dies if the host dies; on non-Linux
//! we fall back to kill-on-drop.
//!
//! This is the production launcher; in-process tests drive the handshake via a
//! spawned task instead (no real subprocess).

use std::process::{Child, Command, Stdio};

/// Spawns the plugin at `path`, wiring the engine address env var and the
/// parent-death signal (Linux) / kill-on-drop (non-Linux).
///
/// Returns the spawned [`Child`]; the host keeps it alive for the plugin's
/// lifetime and kills it on shutdown.
///
/// # Errors
/// Returns the underlying [`std::io::Error`] if the process could not be
/// spawned.
pub fn spawn_plugin(path: &std::path::Path, engine_addr: &str) -> std::io::Result<ChildGuard> {
    let mut cmd = Command::new(path);
    cmd.env(crate::ENGINE_ADDRESS_KEY, engine_addr)
        // Capture the child's stdio so the host can forward it to its log
        // (Go: the runtime tees child stdout/stderr).
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    set_pdeathsig(&mut cmd);

    Ok(ChildGuard(cmd.spawn()?))
}

/// Owns a spawned plugin and **kills it on drop**. On Linux this complements the
/// `PR_SET_PDEATHSIG` parent-death signal; on non-Linux it is the sole safety
/// net ensuring an orphaned plugin does not outlive its host.
#[derive(Debug)]
pub struct ChildGuard(Child);

impl ChildGuard {
    /// Borrows the underlying child (e.g. to drain its piped stdout/stderr).
    #[must_use]
    pub fn child(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Best-effort kill; the process may already have exited.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// On Linux, arm `PR_SET_PDEATHSIG = SIGTERM` in the child via `pre_exec`.
#[cfg(target_os = "linux")]
fn set_pdeathsig(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: `pre_exec` runs in the forked child *before* `execve`. Between
    // `fork` and `execve` only async-signal-safe operations are permitted.
    // `prctl(PR_SET_PDEATHSIG, SIGTERM)` is a single async-signal-safe syscall
    // that takes no locks and allocates nothing; it merely registers that the
    // kernel should deliver SIGTERM to this child when its parent thread dies.
    // We do not touch any shared state, so this is sound (00 §7.6).
    #[allow(unsafe_code)]
    unsafe {
        cmd.pre_exec(|| {
            let rc = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Non-Linux: no parent-death signal; the host relies on kill-on-drop.
#[cfg(not(target_os = "linux"))]
fn set_pdeathsig(_cmd: &mut Command) {}
