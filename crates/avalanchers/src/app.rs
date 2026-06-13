// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `avalanchers` application driver — the Rust port of Go `app/app.go`
//! (`New` / `Run`) plus the `version.GetVersions()` helper from
//! `version/string.go`.
//!
//! `main.rs` is a thin shell over this module: it builds the clap command,
//! handles the printed-and-quit flags (`--version` / `--version-json`), resolves
//! the [`ava_config::node::Config`], prints the TTY banner, restricts the
//! data/log directory permissions, builds the [`LogFactory`], raises the fd
//! limit, builds the single tokio runtime (17 §1.1), assembles the [`Node`],
//! installs the signal handlers (17 §2.5), and blocks on `dispatch`.

// The crate root forbids `unsafe`; the one isolated `libc::setrlimit` call below
// re-enables it locally with a `// SAFETY:` note (mirrors
// `ava-vm-rpc/src/host/subprocess.rs`).

use std::io::IsTerminal;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use ava_config::flags::{self, FLAG_SPECS};
use ava_config::keys;
use ava_config::node::Config;
use ava_config::precedence::Layered;
use ava_node::logging::{self, LogFactory};
use ava_node::node::Node;

/// The ASCII banner printed to a TTY on startup — byte-identical to Go
/// `app.Header` (`app/app.go`).
pub const HEADER: &str = r"     _____               .__                       .__
    /  _  \___  _______  |  | _____    ____   ____ |  |__   ____    ,_ o
   /  /_\  \  \/ /\__  \ |  | \__  \  /    \_/ ___\|  |  \_/ __ \   / //\,
  /    |    \   /  / __ \|  |__/ __ \|   |  \  \___|   Y  \  ___/    \>> |
  \____|__  /\_/  (____  /____(____  /___|  /\___  >___|  /\___  >    \\
          \/           \/          \/     \/     \/     \/     \/";

/// The versions relevant to a build of `avalanchers`, mirroring Go
/// `version.Versions` (`version/string.go`). Serializes to the same JSON shape
/// as Go's `--version-json`, so the output is drop-in unmarshalable.
///
/// `application` carries the avalanchego-compatible identity (Go
/// `Current.String()` → `avalanchego/1.14.2`); the wire/P2P client name stays
/// `avalanchego` for interop (see `version_string` in `main.rs`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Versions {
    /// The avalanchego-compatible application version string (`Current.String()`).
    pub application: String,
    /// The on-disk database schema version (`CurrentDatabase`).
    pub database: String,
    /// The rpcchainvm plugin protocol version (`RPCChainVMProtocol`).
    pub rpcchainvm: u64,
    /// The git commit this binary was built from; empty if unknown.
    pub commit: String,
    /// The toolchain version, with any `go`/`rustc` prefix trimmed.
    pub go: String,
}

impl Versions {
    /// The human-readable `--version` line. Mirrors Go `Versions.String()`
    /// (`<app> [database=..., rpcchainvm=..., (commit=...,) go=...]`) but
    /// prefixes the local `avalanchers/<semver>` identity so the local CLI is
    /// distinguishable from the avalanchego version it tracks (the M0
    /// invariant — the substring `avalanchers/` must appear).
    #[must_use]
    pub fn line(&self) -> String {
        let v = &*ava_version::CURRENT;
        let mut s = format!(
            "avalanchers/{}.{}.{} [application={}, database={}, rpcchainvm={}, ",
            v.major, v.minor, v.patch, self.application, self.database, self.rpcchainvm
        );
        if !self.commit.is_empty() {
            s.push_str(&format!("commit={}, ", self.commit));
        }
        s.push_str(&format!("go={}]", self.go));
        s
    }
}

/// Build the [`Versions`] for this binary (Go `version.GetVersions()`).
///
/// `application` = `ava_version::CURRENT` display; `database` =
/// [`ava_version::CURRENT_DATABASE`]; `rpcchainvm` =
/// [`ava_version::RPC_CHAIN_VM_PROTOCOL`]; `commit` from the `AVALANCHERS_GIT_COMMIT`
/// build env (empty if unset — the build script may inject it, like Go's
/// `-ldflags -X version.GitCommit`); `go` carries the toolchain identity
/// (`rustc` version when injected, else empty).
#[must_use]
pub fn versions() -> Versions {
    Versions {
        application: ava_version::CURRENT.display(),
        database: ava_version::CURRENT_DATABASE.to_string(),
        rpcchainvm: u64::from(ava_version::RPC_CHAIN_VM_PROTOCOL),
        commit: option_env!("AVALANCHERS_GIT_COMMIT")
            .unwrap_or("")
            .to_string(),
        go: option_env!("AVALANCHERS_RUSTC_VERSION")
            .unwrap_or("")
            .to_string(),
    }
}

/// Resolve the node [`Config`] from `argv` (the `--network-id`, `--config-file`,
/// env, file, default layering). Shared by `main` and the parse-only smoke test
/// so the test never has to spawn the blocking node.
///
/// # Errors
/// Propagates [`ava_config::ConfigError`] from clap parsing, the layered
/// resolver, or `get_node_config` validation.
pub fn build_config(args: impl IntoIterator<Item = String>) -> ava_config::Result<Config> {
    let cmd = flags::build_command(FLAG_SPECS);
    let layered = Layered::build(cmd, args, FLAG_SPECS)?;
    ava_config::parse::get_node_config(&layered)
}

/// Restrict `dir` (and its contents) to user `rwx`, mirroring Go
/// `perms.ChmodR(dir, true, ReadWriteExecute)` (`app.New`). Best-effort on unix;
/// a no-op elsewhere. Errors are returned so `main` can surface them like Go.
///
/// # Errors
/// Any `std::io` / walk error from reading the tree or setting permissions.
#[cfg(unix)]
pub fn chmod_r(dir: &str) -> std::io::Result<()> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    let path = Path::new(dir);
    if !path.exists() {
        return Ok(());
    }
    let mode = 0o700;
    // Set the root, then walk children (a missing/empty tree is fine).
    let set = |p: &Path| -> std::io::Result<()> {
        let perms = fs::Permissions::from_mode(mode);
        fs::set_permissions(p, perms)
    };
    set(path)?;
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let child = entry.path();
            if child.is_dir() {
                // Recurse via the public entrypoint on the child path string.
                chmod_r(&child.to_string_lossy())?;
            } else {
                set(&child)?;
            }
        }
    }
    Ok(())
}

/// Non-unix: directory permissions are not POSIX `rwx`; no-op (Go's `perms`
/// package is also unix-shaped).
#[cfg(not(unix))]
pub fn chmod_r(_dir: &str) -> std::io::Result<()> {
    Ok(())
}

/// Raise the process open-file-descriptor soft limit toward `limit`, mirroring
/// Go `ulimit.Set(fdLimit, log)` (`app.New`). Best-effort on unix; a no-op
/// elsewhere. Never lowers a higher existing soft limit and never exceeds the
/// hard limit (clamps, like Go).
#[cfg(unix)]
pub fn set_fd_limit(limit: u64) {
    // SAFETY: `getrlimit`/`setrlimit(RLIMIT_NOFILE, …)` are plain libc syscalls
    // operating on a stack-local `rlimit` struct we fully initialize; they take
    // no locks, touch no shared Rust state, and we check the return code. This
    // is the single isolated FFI in the binary (00 §7.6), scoped exactly like
    // the `prctl` call in `ava-vm-rpc/src/host/subprocess.rs`.
    #[allow(unsafe_code)]
    unsafe {
        let mut rlim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) != 0 {
            return;
        }
        // Clamp the desired soft limit to the hard limit; never lower it.
        let hard = rlim.rlim_max;
        let desired = limit as libc::rlim_t;
        let target = desired.min(hard).max(rlim.rlim_cur);
        if target == rlim.rlim_cur {
            return;
        }
        rlim.rlim_cur = target;
        let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &rlim);
    }
}

/// Non-unix: no rlimit concept here; no-op.
#[cfg(not(unix))]
pub fn set_fd_limit(_limit: u64) {}

/// `true` if stdout is a terminal (Go `term.IsTerminal(os.Stdout.Fd())`).
#[must_use]
pub fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// Install the termination + stack-trace signal handlers, mirroring Go
/// `app.Run`'s signal goroutines (17 §2.5):
/// - `SIGINT`/`SIGTERM` → `node.shutdown(0)` (graceful);
/// - `SIGABRT` → dump every thread/task backtrace to stderr.
///
/// Spawns detached tasks on the ambient runtime; they live for the process.
#[cfg(unix)]
pub fn install_signal_handlers(node: Arc<Node>) {
    use tokio::signal::unix::{SignalKind, signal};

    // SIGINT + SIGTERM → graceful shutdown(0).
    let term_node = Arc::clone(&node);
    tokio::spawn(async move {
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
        term_node.shutdown(0).await;
    });

    // SIGABRT → dump backtraces to stderr (mirror Go `utils.GetStacktrace`).
    tokio::spawn(async move {
        let mut sigabrt = match signal(SignalKind::from_raw(libc::SIGABRT)) {
            Ok(s) => s,
            Err(_) => return,
        };
        loop {
            if sigabrt.recv().await.is_none() {
                return;
            }
            dump_backtrace();
        }
    });
}

/// Non-unix: only ctrl-c maps to a graceful shutdown.
#[cfg(not(unix))]
pub fn install_signal_handlers(node: Arc<Node>) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            node.shutdown(0).await;
        }
    });
}

/// Dump a backtrace of the current thread to stderr (best-effort analogue of
/// Go `utils.GetStacktrace(true)`; Rust cannot enumerate every task's stack, so
/// we capture the handler thread's backtrace, which is enough to point an
/// operator at the recovery playbook).
fn dump_backtrace() {
    let bt = std::backtrace::Backtrace::force_capture();
    eprintln!("SIGABRT received — backtrace:\n{bt}");
}

/// Build the [`LogFactory`] from the resolved logging block (Go
/// `logging.NewFactory(config.LoggingConfig)`).
///
/// # Errors
/// Propagates the logging-init error (bad log directory, rotater config, …)
/// wrapped in [`anyhow::Error`] for the binary's top-level context.
pub fn build_log_factory(cfg: &Config) -> anyhow::Result<Arc<LogFactory>> {
    let handles = logging::init(&cfg.logging_config)?;
    Ok(Arc::new(LogFactory::new(
        cfg.logging_config.clone(),
        handles,
    )))
}

/// `true` if the resolved config asked for a printed-and-quit version flag — a
/// convenience the smoke test can assert without running the node.
#[must_use]
pub fn version_flags(layered: &Layered) -> (bool, bool) {
    (
        layered.get_bool(keys::KEY_VERSION).unwrap_or(false),
        layered.get_bool(keys::KEY_VERSION_JSON).unwrap_or(false),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse-only smoke: the layered resolver + `get_node_config` succeed for
    /// the implicit mainnet default and for `--network-id=fuji` (12 §9). We do
    /// not spawn the node (that blocks); we only prove the config builds.
    ///
    /// `get_node_config` materializes the staking TLS cert; to keep the test
    /// hermetic (no shared `$HOME/.avalanchego` writes) we point `--data-dir`
    /// at a fresh temp dir and use the in-memory ephemeral cert (the same flag
    /// CI/dev nodes use). Both knobs are real flags, so the layering path is
    /// still exercised end-to-end.
    #[test]
    fn build_config_for_mainnet_and_fuji() {
        let tmp =
            std::env::temp_dir().join(format!("avalanchers-m8_31-smoke-{}", std::process::id()));
        let data_dir = tmp.to_string_lossy().into_owned();
        let common = || {
            vec![
                "avalanchers".to_string(),
                format!("--data-dir={data_dir}"),
                "--staking-ephemeral-cert-enabled=true".to_string(),
                "--staking-ephemeral-signer-enabled=true".to_string(),
            ]
        };

        let mut mainnet_args = common();
        let mainnet = build_config(mainnet_args.drain(..));
        assert!(
            mainnet.is_ok(),
            "build_config(mainnet) succeeds, got {:?}",
            mainnet.err()
        );
        assert_eq!(
            mainnet.unwrap().network_id,
            1,
            "default network is mainnet (1)"
        );

        let mut fuji_args = common();
        fuji_args.push("--network-id=fuji".to_string());
        let fuji = build_config(fuji_args.drain(..));
        assert!(
            fuji.is_ok(),
            "build_config(--network-id=fuji) succeeds, got {:?}",
            fuji.err()
        );
        assert_eq!(fuji.unwrap().network_id, 5, "fuji network id is 5");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `versions()` is serde-serializable into the Go `--version-json` shape and
    /// the human line carries the `avalanchers/` identity (M0 invariant).
    #[test]
    fn versions_shape_and_line() {
        let v = versions();
        let json = serde_json::to_value(&v).expect("serialize versions");
        for field in ["application", "database", "rpcchainvm", "commit", "go"] {
            assert!(
                json.get(field).is_some(),
                "version-json has field {field:?}, got {json:?}"
            );
        }
        assert!(
            v.line().contains("avalanchers/"),
            "version line carries avalanchers/, got {:?}",
            v.line()
        );
        assert!(
            v.line().contains("database="),
            "version line carries Go-style detail, got {:?}",
            v.line()
        );
    }
}
