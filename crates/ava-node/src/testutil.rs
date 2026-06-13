// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Shared test fixtures for the node lifecycle tests (`node`, `dispatch`,
//! `shutdown`).
//!
//! The global `tracing` subscriber can only be installed once per process, so
//! [`log_factory`] installs it exactly once (across every test in the binary)
//! and hands out a shared [`LogFactory`]; per-test config still varies (each
//! test gets its own data dir / process-context path).

use std::sync::{Arc, OnceLock};

use ava_config::flags::{FLAG_SPECS, build_command};
use ava_config::node::Config;
use ava_config::parse::get_node_config;
use ava_config::precedence::Layered;

use crate::logging::LogFactory;
use crate::node::Node;

/// A minimal, network-quiet node config: local network, in-memory DB,
/// ephemeral identity, OS-assigned ports, explicit public IP (so NAT never
/// probes the LAN), a near-zero `http-shutdown-wait` so the order test does not
/// sleep, and the admin + indexer surfaces enabled.
#[must_use]
pub(crate) fn test_config(data_dir: &std::path::Path) -> Config {
    let args: Vec<String> = [
        "avalanchers",
        "--network-id=local",
        "--db-type=memdb",
        "--staking-ephemeral-cert-enabled",
        "--staking-ephemeral-signer-enabled",
        "--http-host=127.0.0.1",
        "--http-port=0",
        "--staking-port=0",
        "--public-ip=127.0.0.1",
        "--api-admin-enabled",
        "--index-enabled",
        "--http-shutdown-wait=0s",
    ]
    .into_iter()
    .map(String::from)
    .chain([format!("--data-dir={}", data_dir.display())])
    .collect();

    let layered = Layered::build_with_env(
        build_command(FLAG_SPECS),
        args,
        FLAG_SPECS,
        std::iter::empty(),
    )
    .unwrap_or_else(|e| panic!("Layered::build_with_env(): {e}"));
    get_node_config(&layered).unwrap_or_else(|e| panic!("get_node_config(): {e}"))
}

/// The process-shared logging factory. The global subscriber is installed on
/// the first call; later calls reuse the same [`LogFactory`] (the logging block
/// is identical across the lifecycle tests).
pub(crate) fn log_factory(cfg: &Config) -> Arc<LogFactory> {
    static FACTORY: OnceLock<Arc<LogFactory>> = OnceLock::new();
    Arc::clone(FACTORY.get_or_init(|| {
        let handles = crate::logging::init(&cfg.logging_config)
            .unwrap_or_else(|e| panic!("logging::init(): {e}"));
        Arc::new(LogFactory::new(cfg.logging_config.clone(), handles))
    }))
}

/// Assemble a node from [`test_config`] under `dir`, sharing the process-wide
/// logging factory.
pub(crate) async fn build_node(dir: &std::path::Path) -> Arc<Node> {
    let config = Arc::new(test_config(dir));
    let log_factory = log_factory(&config);
    Node::new(config, log_factory, tokio::runtime::Handle::current())
        .await
        .unwrap_or_else(|e| panic!("Node::new(): {e}"))
}
