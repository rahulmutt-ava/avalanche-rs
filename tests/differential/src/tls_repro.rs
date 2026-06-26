// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Isolated rustls↔Go TLS-1.3 handshake repro driver (M9.15). Spawns the Go
//! `tls_handshake` harness (Task 1) and drives `ava_network`'s `Upgrader`
//! against it to localize the live `mixed_network` TLS stall. See
//! `docs/superpowers/specs/2026-06-24-m9.15-tls-handshake-repro-design.md`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use ava_network::Identity;
use ava_network::peer::tls_config;
use ava_network::peer::upgrader::Upgrader;
use rustls::ClientConfig;
use rustls::crypto::ring::default_provider;
use serde::Deserialize;
use tokio::net::{TcpListener, TcpStream};

/// Parsed JSON outcome emitted by the Go harness (Task 1).
#[derive(Debug, Clone, Deserialize)]
pub struct GoOutcome {
    /// Whether the TLS handshake succeeded on the Go side.
    pub ok: bool,
    /// Verbatim Go error string when `ok` is false.
    #[serde(default)]
    pub error: Option<String>,
    /// Negotiated TLS version (e.g. `772` = TLS 1.3).
    #[serde(default)]
    pub version: Option<u16>,
    /// Negotiated cipher suite ID (e.g. `4865` = `TLS_AES_128_GCM_SHA256`).
    #[serde(default)]
    pub cipher_suite: Option<u16>,
    /// Number of DER certificates in the peer certificate chain.
    #[serde(default)]
    pub peer_cert_len: Option<usize>,
    /// Public-key algorithm of the peer leaf cert (e.g. `"ecdsa"`).
    #[serde(default)]
    pub peer_key_type: Option<String>,
}

/// Parse the Go harness's single JSON outcome line (last non-empty stdout line).
///
/// # Errors
/// Returns `Err` if stdout is empty or the last non-empty line is not valid JSON.
pub fn parse_go_outcome(stdout: &str) -> Result<GoOutcome, String> {
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| "empty Go harness stdout".to_string())?;
    serde_json::from_str(line.trim()).map_err(|e| format!("parse outcome {line:?}: {e}"))
}

/// The avalanchego source checkout dir (`$AVALANCHEGO_SRC`, else `$HOME/avalanchego`).
#[must_use]
pub fn go_src_dir() -> PathBuf {
    if let Ok(src) = std::env::var("AVALANCHEGO_SRC") {
        return PathBuf::from(src);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("avalanchego")
}

/// Whether the live Go binary is configured (live-arm gate).
#[must_use]
pub fn avalanchego_available() -> bool {
    std::env::var("AVALANCHEGO_PATH").is_ok()
}

/// Copy `main.go` into the Go checkout and `go build` it; returns the binary path.
///
/// The Go binary is resolved from `$TLS_REPRO_GO` if set, otherwise falls back
/// to `"go"` on `PATH`. Set `TLS_REPRO_GO` to the nix-shell go binary when the
/// default `go` on PATH is a different toolchain version than the checkout expects.
///
/// # Errors
/// Returns `Err` if the source dir cannot be created, `main.go` cannot be
/// copied, or `go build` fails.
pub fn build_go_harness() -> Result<PathBuf, String> {
    let src = go_src_dir();
    let pkg = src.join("tests").join("tls_handshake_repro");
    std::fs::create_dir_all(&pkg).map_err(|e| format!("mkdir {pkg:?}: {e}"))?;
    let canonical =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("go-oracle/tls_handshake/main.go");
    std::fs::copy(&canonical, pkg.join("main.go")).map_err(|e| format!("copy main.go: {e}"))?;
    let out = src.join("target-tls-handshake");
    let go_bin = std::env::var("TLS_REPRO_GO").unwrap_or_else(|_| "go".to_string());
    let status = std::process::Command::new(&go_bin)
        .current_dir(&src)
        .args(["build", "-o"])
        .arg(&out)
        .arg("./tests/tls_handshake_repro")
        .status()
        .map_err(|e| format!("spawn {go_bin} build: {e}"))?;
    if !status.success() {
        return Err(format!("go build failed: {status}"));
    }
    Ok(out)
}

/// A diagnostic client config identical to production except `key_log` is wired
/// to `SSLKEYLOGFILE`. DIAGNOSTIC ONLY — never used by a real node.
///
/// # Errors
/// Returns `Err` if the underlying production TLS config cannot be built.
pub fn diagnostic_client_config(id: &Identity) -> Result<Arc<ClientConfig>, String> {
    // Reuse the production builder, then clone its inner config to set key_log.
    let base = tls_config::client_config(id).map_err(|e| e.to_string())?;
    let mut cfg = (*base).clone();
    // Ensure the ring provider is installed so KeyLogFile can hash secrets.
    let _ = default_provider();
    cfg.key_log = Arc::new(rustls::KeyLogFile::new());
    Ok(Arc::new(cfg))
}

/// Dial `addr` and run the client-side TLS upgrade; returns the derived peer
/// NodeID string on success or the verbatim error string on failure.
///
/// # Errors
/// Returns `Err` with the TCP dial or TLS handshake error message.
pub async fn rust_client_upgrade(addr: SocketAddr, id: &Identity) -> Result<String, String> {
    let tcp = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("dial {addr}: {e}"))?;
    let cfg = tls_config::client_config(id).map_err(|e| e.to_string())?;
    match Upgrader::client(cfg).upgrade(tcp).await {
        Ok((node_id, _tls, _cert)) => Ok(node_id.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Accept one connection on `listener` and run the server-side TLS upgrade;
/// returns the derived peer NodeID string on success or the verbatim error string.
///
/// # Errors
/// Returns `Err` with the accept or TLS handshake error message.
pub async fn rust_server_upgrade(listener: TcpListener, id: &Identity) -> Result<String, String> {
    let (tcp, _peer) = listener
        .accept()
        .await
        .map_err(|e| format!("accept: {e}"))?;
    let cfg = tls_config::server_config(id).map_err(|e| e.to_string())?;
    match Upgrader::server(cfg).upgrade(tcp).await {
        Ok((node_id, _tls, _cert)) => Ok(node_id.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_success_outcome_line() {
        let line = r#"{"ok":true,"version":772,"cipher_suite":4865,"peer_cert_len":1,"peer_key_type":"ecdsa"}"#;
        let o = parse_go_outcome(line).expect("parse success line");
        assert!(o.ok, "ok flag");
        assert_eq!(o.version, Some(772), "tls1.3 version");
        assert_eq!(o.peer_key_type.as_deref(), Some("ecdsa"), "peer key type");
    }

    #[test]
    fn parses_a_failure_outcome_and_ignores_stderr_noise() {
        // Real stdout may carry only the JSON line; the driver takes the last
        // non-empty line so leading blank lines are tolerated.
        let stdout = "\n{\"ok\":false,\"error\":\"remote error: tls: bad certificate\"}\n";
        let o = parse_go_outcome(stdout).expect("parse failure line");
        assert!(!o.ok, "ok=false");
        assert_eq!(
            o.error.as_deref(),
            Some("remote error: tls: bad certificate"),
            "verbatim error preserved",
        );
    }

    #[test]
    fn parse_rejects_non_json() {
        assert!(
            parse_go_outcome("not json at all").is_err(),
            "non-json rejected"
        );
    }

    #[test]
    fn go_src_dir_defaults_under_home() {
        // With AVALANCHEGO_SRC unset, falls back to $HOME/avalanchego.
        let dir = go_src_dir();
        assert!(
            dir.ends_with("avalanchego"),
            "default src dir ends in avalanchego, got {dir:?}"
        );
    }
}
