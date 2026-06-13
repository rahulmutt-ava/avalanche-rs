// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `avalanchers` binary entrypoint — the Rust port of Go `main/main.go`.
//!
//! The flow mirrors `main.go` + `app.Run` (specs/12 §9, 17 §1.1/§2.5/§5):
//! register EVM extras → build the clap command → handle the printed-and-quit
//! flags (`--version` / `--version-json` / `--help`) → resolve the node
//! [`Config`](ava_config::node::Config) → print the TTY banner → restrict the
//! data/log dir permissions → build the
//! [`LogFactory`](ava_node::logging::LogFactory) → raise the fd limit → build
//! the **single** process-wide tokio runtime → assemble the
//! [`Node`](ava_node::node::Node) → install the signal handlers → block on
//! `dispatch` → exit with `node.exit_code()`.
//!
//! All `unsafe` lives in the library crate root (`lib.rs`); the one isolated
//! `setrlimit` FFI is [`avalanchers::app::set_fd_limit`]. This binary is
//! `unsafe`-free.

#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::error::ErrorKind;

use ava_config::flags::{self, FLAG_SPECS};
use ava_config::keys;
use ava_config::precedence::Layered;
use avalanchers::app;

/// Local build identity reported by `--version`, in `client/maj.min.patch` form.
///
/// This is the *local CLI* identity (`avalanchers/...`). The numeric version is
/// sourced from `ava_version::CURRENT` (the avalanchego version this node is
/// compatible with). The *wire/P2P* client string this node advertises during
/// the handshake stays `avalanchego` for drop-in interop — that is a separate
/// constant (`ava_version::CLIENT`, see specs/26-versioning-and-compatibility.md
/// and specs/03-core-primitives.md §5.1).
fn version_string() -> String {
    let v = &*ava_version::CURRENT;
    format!("avalanchers/{}.{}.{}", v.major, v.minor, v.patch)
}

fn main() -> ExitCode {
    // 1. Register EVM extras (Go `evm.RegisterAllLibEVMExtras()`). The Rust EVM
    //    is reth; it registers its precompiles/state hooks at chain-creation
    //    time inside `ava-evm`, so there is no process-global init to call here.
    //    Kept as a documented no-op to preserve the Go step ordering.

    let args: Vec<String> = std::env::args().collect();

    // 2. Build the clap command from the flag table (12 §1.4) and let clap
    //    render `--help` (and surface parse errors) the way pflag does: clap
    //    returns `Err(DisplayHelp)` after printing help to stdout → exit 0.
    let cmd = flags::build_command(FLAG_SPECS);
    if let Err(e) = cmd.clone().try_get_matches_from(&args) {
        let _ = e.print();
        return match e.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        };
    }

    // 3. Resolve the layered config so the printed-and-quit version flags read
    //    through the same flag>env>file>default precedence as everything else.
    let layered = match Layered::build(cmd, args, FLAG_SPECS) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("couldn't configure flags: {e}");
            return ExitCode::FAILURE;
        }
    };

    // 3a. `--version` / `--version-json` (printed-and-quit). Both set → error.
    let want_version = layered.get_bool(keys::KEY_VERSION).unwrap_or(false);
    let want_version_json = layered.get_bool(keys::KEY_VERSION_JSON).unwrap_or(false);
    if want_version && want_version_json {
        eprintln!("can't print both JSON and human readable versions");
        return ExitCode::FAILURE;
    }
    if want_version_json {
        return match serde_json::to_string_pretty(&app::versions()) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("couldn't marshal versions: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if want_version {
        // Carries `avalanchers/<semver>` (M0 invariant) + the Go-style
        // `[application=…, database=…, rpcchainvm=…, go=…]` detail.
        debug_assert_eq!(
            app::versions().application,
            version_string().replacen("avalanchers/", "avalanchego/", 1),
            "version line tracks the same semver as the local identity"
        );
        println!("{}", app::versions().line());
        return ExitCode::SUCCESS;
    }

    // 4. Build the resolved node `Config`.
    let config = match ava_config::parse::get_node_config(&layered) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("couldn't load node config: {e}");
            return ExitCode::FAILURE;
        }
    };

    // The rest mirrors `app.New` + `app.Run`; surface the first error like Go.
    match run(config) {
        Ok(code) => ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1)),
        Err(e) => {
            eprintln!("couldn't start node: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// `app.New` + `app.Run`: banner, chmod, log factory, fd limit, runtime, node
/// assembly, signal handlers, dispatch. Returns the node exit code.
fn run(config: ava_config::node::Config) -> anyhow::Result<i32> {
    // 5. TTY banner (Go `term.IsTerminal(os.Stdout.Fd())` → print `app.Header`).
    if app::stdout_is_tty() {
        println!("{}", app::HEADER);
    }

    // 6. Restrict data/log dir permissions to user-rwx (Go `perms.ChmodR`).
    app::chmod_r(&config.database_config.path)
        .context("failed to restrict the permissions of the database directory")?;
    app::chmod_r(&config.logging_config.directory)
        .context("failed to restrict the permissions of the log directory")?;

    // Build the LogFactory (Go `logging.NewFactory`), then raise the fd limit
    // (Go `ulimit.Set(config.FdLimit, log)`).
    let log_factory = app::build_log_factory(&config).context("failed to initialize log")?;
    app::set_fd_limit(config.fd_limit);

    // 7. The single process-wide multi-thread runtime (17 §1.1). No library
    //    crate builds its own; `Node::new` takes this `Handle`.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ava-worker")
        .build()
        .context("build tokio runtime")?;
    let handle = rt.handle().clone();

    let config = Arc::new(config);
    rt.block_on(async move {
        let node = ava_node::node::Node::new(config, log_factory, handle)
            .await
            .context("failed to initialize node")?;
        app::install_signal_handlers(Arc::clone(&node));
        // `dispatch` blocks until the P2P stack stops, then returns the exit
        // code recorded by the first fatal `shutdown(code)` (17 §5).
        let code = Arc::clone(&node).dispatch().await;
        Ok::<i32, anyhow::Error>(code)
    })
}
