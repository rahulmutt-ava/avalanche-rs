// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Live mixed-network role model and CLI flag-vector assembly.
//!
//! Used by the M9.15 two-binary harness to launch `avalanchego` (Go) and
//! `avalanchers` (Rust) with identical topology flags so they form a single
//! local network.

use std::net::TcpListener;
use std::path::PathBuf;

use ava_crypto::secp256k1::PrivateKey;
use ava_evm_reth::{
    Address, EvmSignature, RlpEncodable, SignableTransaction, TransactionSigned, TxKind, TxLegacy,
    U256,
};
use serde_json::Value;

use crate::network::NetworkError;
use crate::rpc;

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
    /// Empty for a beacon / seed; one or more peers for a follower or a
    /// mesh member. Rendered as comma-joined `--bootstrap-ips`/`--bootstrap-ids`.
    pub bootstrap: Vec<Bootstrap>,
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
        // In-memory DB on both nodes: the ephemeral test net keeps no state
        // between runs, and the `avalanchers` release build ships without the
        // optional `rocksdb` backend the default on-disk `leveldb` requires
        // (M9.15 gap note). Go honors `--db-type=memdb` identically.
        "--db-type=memdb".to_owned(),
        // Surface the full handshake ladder (the post-TLS upgrader + finish
        // rungs log at debug; Task-6/M9.15 D1). Go honors `--log-level=debug`
        // identically — this only widens the captured `logs/main.log`, it does
        // not change node behavior.
        "--log-level=debug".to_owned(),
    ];
    if !launch.bootstrap.is_empty() {
        let ips = launch
            .bootstrap
            .iter()
            .map(|b| b.ip.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let ids = launch
            .bootstrap
            .iter()
            .map(|b| b.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        args.push(format!("--bootstrap-ips={ips}"));
        args.push(format!("--bootstrap-ids={ids}"));
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
    local_staker_in(std::path::Path::new(&src), idx)
}

/// Resolve staker `idx` under an explicit source root (the testable core of
/// [`local_staker`]; kept private so the env-reading wrapper is the public API).
fn local_staker_in(src: &std::path::Path, idx: u8) -> Result<CertPair, NetworkError> {
    let dir = src.join("staking").join("local");
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

/// Generate a fresh ECDSA-P256 staking cert/key (the only format `avalanchers`
/// supports) and write it under `dir` as `<name>.crt` / `<name>.key`.
///
/// The Go beacon must present a genesis initial-staker cert (RSA `staker1`), but
/// the Rust follower is a non-validating bootstrapper, so its node-ID need not be
/// a genesis staker. `avalanchers`' staking identity only loads ECDSA-P256 keys
/// (`ava-network`'s `Identity::from_pem` rejects the RSA local staker keys that
/// Go accepts — see the M9.15 gap note), so the follower gets a freshly generated
/// ECDSA cert here rather than the RSA `staker2`.
///
/// # Errors
/// Returns [`NetworkError::CertSource`] if cert generation or writing fails.
pub fn generate_staker(dir: &std::path::Path, name: &str) -> Result<CertPair, NetworkError> {
    let (cert_pem, key_pem) = ava_crypto::staking::new_cert_and_key_bytes()
        .map_err(|e| NetworkError::CertSource(format!("generate staker cert: {e}")))?;
    let cert = dir.join(format!("{name}.crt"));
    let key = dir.join(format!("{name}.key"));
    ava_crypto::staking::write_cert_and_key(&cert, &key, &cert_pem, &key_pem)
        .map_err(|e| NetworkError::CertSource(format!("write staker cert: {e}")))?;
    Ok(CertPair { cert, key })
}

/// Pull `nodeID` from an `info.getNodeID` result.
#[must_use]
pub fn parse_node_id(v: &serde_json::Value) -> Option<String> {
    v.get("nodeID").and_then(|n| n.as_str()).map(str::to_owned)
}

/// Pull `isBootstrapped` from an `info.isBootstrapped` result.
#[must_use]
pub fn parse_bootstrapped(v: &serde_json::Value) -> Option<bool> {
    v.get("isBootstrapped").and_then(serde_json::Value::as_bool)
}

/// Query `info.getNodeID` over the node's API.
///
/// # Errors
/// Returns [`crate::observation::ObsError`] on URL parse failure, transport
/// error, or a response missing the `nodeID` field.
pub async fn scrape_node_id(api_base: &str) -> Result<String, crate::observation::ObsError> {
    let ep = rpc::Endpoint::parse(api_base)?;
    let res = rpc::call(&ep, "/ext/info", "info.getNodeID", "{}").await?;
    parse_node_id(&res).ok_or_else(|| {
        crate::observation::ObsError::Rpc("info.getNodeID: missing nodeID".to_owned())
    })
}

/// Poll `info.isBootstrapped` for every chain alias until all report true or
/// `within` elapses.
///
/// # Errors
/// Returns [`NetworkError::Timeout`] if not all chains bootstrap within
/// `within`, or if `api_base` is not a valid `http://host:port` URL.
pub async fn await_bootstrapped(
    api_base: &str,
    chains: &[&str],
    within: std::time::Duration,
) -> Result<(), NetworkError> {
    let ep = rpc::Endpoint::parse(api_base)
        .map_err(|e| NetworkError::Timeout(format!("bad api_base {api_base}: {e}")))?;
    let deadline = std::time::Instant::now()
        .checked_add(within)
        .ok_or_else(|| NetworkError::Timeout("deadline overflow".to_owned()))?;
    loop {
        let mut all = true;
        for chain in chains {
            let params = format!(r#"{{"chain":"{chain}"}}"#);
            let ready = rpc::call(&ep, "/ext/info", "info.isBootstrapped", &params)
                .await
                .ok()
                .and_then(|v| parse_bootstrapped(&v))
                .unwrap_or(false);
            if !ready {
                all = false;
                break;
            }
        }
        if all {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(NetworkError::Timeout(format!(
                "node {api_base} did not bootstrap {chains:?} within {within:?}"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// The five well-known `local`-network initial-staker NodeIDs, in `staker1..5`
/// order — must match `crates/ava-genesis/data/genesis_local.json`
/// `initialStakers`. Fixed constants (the local genesis never changes);
/// `boot_mixed` sanity-checks index 0 against a live `info.getNodeID` scrape.
pub const LOCAL_VALIDATOR_NODE_IDS: [&str; 5] = [
    "NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg",
    "NodeID-MFrZFVCXPv5iCn6M9K6XduxGTYp891xXZ",
    "NodeID-NFBbbJ4qCmNaCzeW7sxErhvWqvEQMnYcN",
    "NodeID-GWPcbFJZFfZreETSoWjPimr846mXEKCtu",
    "NodeID-P7oB2McjBGgW2NXXWVYjV8JEDFoW9xDE5",
];

/// Local C-chain ID for the Avalanche `local` network (`--network-id=local`).
const LOCAL_CHAIN_ID: u64 = 43_112;

/// EIP-155 gas limit for a simple value transfer.
const TRANSFER_GAS: u64 = 21_000;

/// The well-known "ewoq" pre-funded private key on `local` networks.
///
/// Address: `0x8db97C7cEcE249c2b98bDC0226Cc4C2A57BF52FC`
const EWOQ_KEY_HEX: &str = "56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027";

/// Decide whether two polled C-chain heights mean "settled at the same tip".
///
/// Returns `true` iff both `a` and `b` are `Some(h)` with `h == a` and `h >= min`.
#[must_use]
pub fn settled(a: Option<u64>, b: Option<u64>, min: u64) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x == y && x >= min)
}

/// Parse an `eth_blockNumber` hex-quantity result (`"0x1a"`) into a height.
#[must_use]
pub fn parse_eth_block_number(v: &Value) -> Option<u64> {
    let s = v.as_str()?.strip_prefix("0x")?;
    u64::from_str_radix(s, 16).ok()
}

/// Poll both nodes' C-chain `eth_blockNumber` until equal, `>= min`, and stable
/// across two consecutive polls (to guard against transient mid-advance reads).
///
/// # Errors
/// Returns [`NetworkError::Timeout`] if the heights do not converge within
/// `within`, or if either `api_base` is not a valid `http://host:port` URL.
pub async fn await_same_c_height(
    a_api: &str,
    b_api: &str,
    min: u64,
    within: std::time::Duration,
) -> Result<u64, NetworkError> {
    let ea = rpc::Endpoint::parse(a_api).map_err(|e| NetworkError::Timeout(format!("{e}")))?;
    let eb = rpc::Endpoint::parse(b_api).map_err(|e| NetworkError::Timeout(format!("{e}")))?;
    let deadline = std::time::Instant::now()
        .checked_add(within)
        .ok_or_else(|| NetworkError::Timeout("deadline overflow".to_owned()))?;
    let mut last_stable: Option<u64> = None;
    loop {
        let ha = rpc::call(&ea, "/ext/bc/C/rpc", "eth_blockNumber", "[]")
            .await
            .ok()
            .and_then(|v| parse_eth_block_number(&v));
        let hb = rpc::call(&eb, "/ext/bc/C/rpc", "eth_blockNumber", "[]")
            .await
            .ok()
            .and_then(|v| parse_eth_block_number(&v));
        if settled(ha, hb, min) {
            let h = ha.unwrap_or(0);
            if last_stable == Some(h) {
                return Ok(h);
            }
            last_stable = Some(h);
        } else {
            last_stable = None;
        }
        if std::time::Instant::now() >= deadline {
            return Err(NetworkError::Timeout(format!(
                "C-chain heights never settled >= {min} (a={ha:?} b={hb:?})"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Build, sign, and issue one C-chain legacy value transfer from the pre-funded
/// "ewoq" key to itself against `go_api`'s C-chain RPC endpoint, then poll
/// `eth_getTransactionReceipt` until the tx is mined.
///
/// Vehicle: reth/alloy primitives via `ava-evm-reth` for tx construction +
/// RLP encoding; `ava-crypto` secp256k1 for EIP-155 signing.  The signing
/// pattern mirrors `ava-evm/tests/evm_factory.rs::sign_legacy`.
///
/// # Errors
/// Returns [`NetworkError::Timeout`] on any RPC failure, parse error, or if
/// the receipt is not observed within 60 s.
pub async fn drive_c_transfer(go_api: &str) -> Result<(), NetworkError> {
    let ep = rpc::Endpoint::parse(go_api)
        .map_err(|e| NetworkError::Timeout(format!("drive_c_transfer: bad url: {e}")))?;

    // 1. Fetch the current nonce for the ewoq address.
    let ewoq_addr = {
        let key = ewoq_key()?;
        Address::from(key.public_key().eth_address())
    };
    let nonce: u64 = {
        let addr_hex = format!("{ewoq_addr:?}");
        let params = format!(r#"["{addr_hex}","latest"]"#);
        let v = rpc::call(&ep, "/ext/bc/C/rpc", "eth_getTransactionCount", &params)
            .await
            .map_err(|e| NetworkError::Timeout(format!("eth_getTransactionCount: {e}")))?;
        let s = v
            .as_str()
            .and_then(|s| s.strip_prefix("0x"))
            .ok_or_else(|| {
                NetworkError::Timeout("eth_getTransactionCount: unexpected result shape".to_owned())
            })?;
        u64::from_str_radix(s, 16)
            .map_err(|e| NetworkError::Timeout(format!("nonce parse: {e}")))?
    };

    // 2. Fetch the current gas price so the tx is priced correctly.
    let gas_price: u128 = {
        let v = rpc::call(&ep, "/ext/bc/C/rpc", "eth_gasPrice", "[]")
            .await
            .map_err(|e| NetworkError::Timeout(format!("eth_gasPrice: {e}")))?;
        let s = v
            .as_str()
            .and_then(|s| s.strip_prefix("0x"))
            .ok_or_else(|| {
                NetworkError::Timeout("eth_gasPrice: unexpected result shape".to_owned())
            })?;
        // If gas price is zero (pre-AP3 genesis), use a nominal 1 nAVAX.
        let raw = u128::from_str_radix(s, 16)
            .map_err(|e| NetworkError::Timeout(format!("gas price parse: {e}")))?;
        if raw == 0 { 1_000_000_000 } else { raw }
    };

    // 3. Build and sign a legacy self-transfer (value=0; a no-op that still
    //    produces a finalized block when mined).
    let raw_tx = build_signed_raw_tx(nonce, gas_price, ewoq_addr)?;

    // 4. Issue via eth_sendRawTransaction.
    let tx_hash: String = {
        let hex = format!("0x{}", hex::encode(&raw_tx));
        let params = format!(r#"["{hex}"]"#);
        let v = rpc::call(&ep, "/ext/bc/C/rpc", "eth_sendRawTransaction", &params)
            .await
            .map_err(|e| NetworkError::Timeout(format!("eth_sendRawTransaction: {e}")))?;
        v.as_str().map(str::to_owned).ok_or_else(|| {
            NetworkError::Timeout("eth_sendRawTransaction: expected string tx hash".to_owned())
        })?
    };

    // 5. Poll eth_getTransactionReceipt until the tx is mined (up to 60 s).
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(60))
        .ok_or_else(|| NetworkError::Timeout("deadline overflow".to_owned()))?;
    loop {
        let params = format!(r#"["{tx_hash}"]"#);
        let v = rpc::call(&ep, "/ext/bc/C/rpc", "eth_getTransactionReceipt", &params)
            .await
            .ok();
        // The result is `null` while pending; any non-null object means mined.
        if let Some(receipt) = v
            && !receipt.is_null()
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(NetworkError::Timeout(format!(
                "tx {tx_hash} not mined within 60 s"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Construct and sign a legacy EIP-155 self-transfer transaction, returning its
/// raw RLP-encoded bytes ready for `eth_sendRawTransaction`.
fn build_signed_raw_tx(nonce: u64, gas_price: u128, to: Address) -> Result<Vec<u8>, NetworkError> {
    let key = ewoq_key()?;

    let tx = TxLegacy {
        chain_id: Some(LOCAL_CHAIN_ID),
        nonce,
        gas_price,
        gas_limit: TRANSFER_GAS,
        to: TxKind::Call(to),
        value: U256::ZERO,
        input: Default::default(),
    };

    // EIP-155 signing: signature_hash() includes chainId in the pre-image when
    // `chain_id` is `Some` (alloy_consensus TxLegacy behavior).
    let sig_hash = tx.signature_hash();
    let rsv = key
        .sign_hash(&sig_hash.0)
        .map_err(|e| NetworkError::Timeout(format!("sign: {e}")))?;
    let r = U256::from_be_slice(&rsv[..32]);
    let s = U256::from_be_slice(&rsv[32..64]);
    let sig = EvmSignature::new(r, s, rsv[64] == 1);
    let signed = TransactionSigned::Legacy(tx.into_signed(sig));

    let mut out = Vec::new();
    signed.encode(&mut out);
    Ok(out)
}

/// Load the well-known ewoq private key.
fn ewoq_key() -> Result<PrivateKey, NetworkError> {
    let bytes = hex::decode(EWOQ_KEY_HEX)
        .map_err(|e| NetworkError::Timeout(format!("ewoq key hex: {e}")))?;
    PrivateKey::from_bytes(&bytes)
        .map_err(|e| NetworkError::Timeout(format!("ewoq key parse: {e}")))
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
                Role::Beacon => Vec::new(),
                Role::Follower => vec![Bootstrap {
                    ip: "127.0.0.1:9651".to_owned(),
                    id: "NodeID-abc".to_owned(),
                }],
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
    fn multi_bootstrapper_args_are_comma_joined() {
        let mut l = launch(Role::Beacon);
        l.bootstrap = vec![
            Bootstrap {
                ip: "127.0.0.1:1".to_owned(),
                id: "NodeID-a".to_owned(),
            },
            Bootstrap {
                ip: "127.0.0.1:2".to_owned(),
                id: "NodeID-b".to_owned(),
            },
        ];
        let args = node_args(&l);
        assert!(
            args.iter()
                .any(|a| a == "--bootstrap-ips=127.0.0.1:1,127.0.0.1:2"),
            "ips comma-joined in order"
        );
        assert!(
            args.iter()
                .any(|a| a == "--bootstrap-ids=NodeID-a,NodeID-b"),
            "ids comma-joined in order"
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

    #[test]
    fn local_staker_missing_dir_errors_with_path() {
        let err = local_staker_in(std::path::Path::new("/nonexistent-xyz"), 1)
            .expect_err("missing cert dir must error");
        assert!(
            format!("{err}").contains("staker1"),
            "error names the cert: {err}"
        );
    }

    #[test]
    fn parse_node_id_extracts_field() {
        let v = serde_json::json!({ "nodeID": "NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg" });
        assert_eq!(
            parse_node_id(&v).as_deref(),
            Some("NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg")
        );
        assert_eq!(parse_node_id(&serde_json::json!({})), None);
    }

    #[test]
    fn parse_bootstrapped_extracts_bool() {
        assert_eq!(
            parse_bootstrapped(&serde_json::json!({ "isBootstrapped": true })),
            Some(true)
        );
        assert_eq!(
            parse_bootstrapped(&serde_json::json!({ "isBootstrapped": false })),
            Some(false)
        );
        assert_eq!(parse_bootstrapped(&serde_json::json!({})), None);
    }

    #[test]
    fn local_validator_node_ids_are_five_distinct() {
        assert_eq!(LOCAL_VALIDATOR_NODE_IDS.len(), 5, "five local validators");
        let mut sorted: Vec<&str> = LOCAL_VALIDATOR_NODE_IDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 5, "node ids are distinct");
        assert_eq!(
            LOCAL_VALIDATOR_NODE_IDS[0], "NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg",
            "staker1 is the first validator (matches the genesis order)"
        );
    }
}

#[cfg(test)]
mod settle_tests {
    use super::settled;
    #[test]
    fn settled_requires_equal_and_min() {
        assert!(settled(Some(3), Some(3), 1), "equal and >= min");
        assert!(!settled(Some(2), Some(3), 1), "unequal heights not settled");
        assert!(!settled(Some(1), Some(1), 2), "below min not settled");
        assert!(!settled(None, Some(1), 1), "missing height not settled");
    }
}
