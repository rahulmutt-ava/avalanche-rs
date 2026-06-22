// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Live mixed-network role model and CLI flag-vector assembly.
//!
//! Used by the M9.15 two-binary harness to launch `avalanchego` (Go) and
//! `avalanchers` (Rust) with identical topology flags so they form a single
//! local network.

use std::net::TcpListener;
use std::path::PathBuf;

use crate::network::NetworkError;

/// The role a node plays in the live mixed net.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Sole initial-staker / validator; proposes and finalizes blocks.
    Beacon,
    /// Non-validating node that bootstraps from (and follows) the beacon.
    Follower,
}

/// A bootstrap target (the beacon's staking address + node ID).
#[derive(Debug, Clone)]
pub struct Bootstrap {
    /// `host:staking_port`, e.g. `127.0.0.1:9651`.
    pub ip: String,
    /// The beacon's scraped `NodeID-...` string.
    pub id: String,
}

/// Everything needed to launch one node (binary path supplied separately).
#[derive(Debug, Clone)]
pub struct NodeLaunch {
    /// HTTP API port.
    pub http_port: u16,
    /// Staking (P2P) port.
    pub staking_port: u16,
    /// Data directory for this node instance.
    pub data_dir: PathBuf,
    /// Path to the TLS staking certificate.
    pub cert_file: PathBuf,
    /// Path to the TLS staking key.
    pub key_file: PathBuf,
    /// `None` for a beacon; `Some` for a follower.
    pub bootstrap: Option<Bootstrap>,
}

/// The exact CLI flag vector for `launch` (mirrors specs/13; both binaries
/// honor these identically).
#[must_use]
pub fn node_args(launch: &NodeLaunch) -> Vec<String> {
    let mut args = vec![
        "--network-id=local".to_owned(),
        format!("--http-port={}", launch.http_port),
        format!("--staking-port={}", launch.staking_port),
        format!("--data-dir={}", launch.data_dir.display()),
        format!("--staking-tls-cert-file={}", launch.cert_file.display()),
        format!("--staking-tls-key-file={}", launch.key_file.display()),
    ];
    if let Some(b) = &launch.bootstrap {
        args.push(format!("--bootstrap-ips={}", b.ip));
        args.push(format!("--bootstrap-ids={}", b.id));
    }
    args
}

/// `n` distinct currently-free localhost TCP ports. Binds `:0`, reads the OS
/// assignment, and drops the listener (a brief TOCTOU window the live arm
/// tolerates — nodes bind immediately after).
///
/// # Errors
/// Returns an `io::Error` if any listener fails to bind or its address cannot
/// be read.
pub fn free_ports(n: usize) -> std::io::Result<Vec<u16>> {
    let mut held = Vec::with_capacity(n);
    let mut ports = Vec::with_capacity(n);
    for _ in 0..n {
        let l = TcpListener::bind(("127.0.0.1", 0))?;
        ports.push(l.local_addr()?.port());
        held.push(l); // hold all until done so we never hand out a duplicate
    }
    Ok(ports)
}

/// A resolved staker cert/key pair.
#[derive(Debug, Clone)]
pub struct CertPair {
    /// Path to the TLS staking certificate file.
    pub cert: PathBuf,
    /// Path to the TLS staking key file.
    pub key: PathBuf,
}

/// Resolve the well-known local staker `idx` (1 or 2) from
/// `$AVALANCHEGO_SRC/staking/local/` (default `~/avalanchego`).
///
/// # Errors
/// Returns [`NetworkError::CertSource`] if the cert or key file is not found.
pub fn local_staker(idx: u8) -> Result<CertPair, NetworkError> {
    let src = std::env::var("AVALANCHEGO_SRC").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/avalanchego")
    });
    let dir = PathBuf::from(&src).join("staking").join("local");
    let cert = dir.join(format!("staker{idx}.crt"));
    let key = dir.join(format!("staker{idx}.key"));
    if !cert.exists() || !key.exists() {
        return Err(NetworkError::CertSource(format!(
            "staker{idx} cert/key not found under {} (set $AVALANCHEGO_SRC)",
            dir.display()
        )));
    }
    Ok(CertPair { cert, key })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn launch(role: Role) -> NodeLaunch {
        NodeLaunch {
            http_port: 9650,
            staking_port: 9651,
            data_dir: PathBuf::from("/tmp/slot0"),
            cert_file: PathBuf::from("/certs/staker1.crt"),
            key_file: PathBuf::from("/certs/staker1.key"),
            bootstrap: match role {
                Role::Beacon => None,
                Role::Follower => Some(Bootstrap {
                    ip: "127.0.0.1:9651".to_owned(),
                    id: "NodeID-abc".to_owned(),
                }),
            },
        }
    }

    #[test]
    fn beacon_args_have_no_bootstrap_flags() {
        let args = node_args(&launch(Role::Beacon));
        assert!(args.iter().any(|a| a == "--network-id=local"), "network-id");
        assert!(args.iter().any(|a| a == "--http-port=9650"), "http-port");
        assert!(
            args.iter().any(|a| a == "--staking-port=9651"),
            "staking-port"
        );
        assert!(
            args.iter()
                .any(|a| a == "--staking-tls-cert-file=/certs/staker1.crt"),
            "cert"
        );
        assert!(
            args.iter()
                .any(|a| a == "--staking-tls-key-file=/certs/staker1.key"),
            "key"
        );
        assert!(
            !args.iter().any(|a| a.starts_with("--bootstrap-ips")),
            "no bootstrap-ips on beacon"
        );
        assert!(
            !args.iter().any(|a| a.starts_with("--bootstrap-ids")),
            "no bootstrap-ids on beacon"
        );
    }

    #[test]
    fn follower_args_carry_bootstrap_topology() {
        let args = node_args(&launch(Role::Follower));
        assert!(
            args.iter().any(|a| a == "--bootstrap-ips=127.0.0.1:9651"),
            "bootstrap-ips"
        );
        assert!(
            args.iter().any(|a| a == "--bootstrap-ids=NodeID-abc"),
            "bootstrap-ids"
        );
    }

    #[test]
    fn free_ports_are_distinct_and_nonzero() {
        let ports = free_ports(4).expect("free_ports");
        assert_eq!(ports.len(), 4, "asked for 4 ports");
        let mut sorted = ports.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "ports are distinct");
        assert!(ports.iter().all(|&p| p != 0), "no zero ports");
    }
}
