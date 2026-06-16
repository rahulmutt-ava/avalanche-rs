// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Live single-node Rust tmpnet driver + `/ext/metrics` scraper (M9.18,
//! specs/02 §10.3).
//!
//! The sustained-load live arm boots one `avalanchers` node, runs a
//! [`crate::generator::LoadGenerator`] against it for `--load-timeout`, then
//! scrapes the merged Prometheus exposition at `/ext/metrics` (specs/18 §1) and
//! checks the SLOs.
//!
//! Like the differential harness's `Network` ([tests/differential/src/network.rs]),
//! this is **non-`cfg`-gated** so it always compiles, but is only *invoked* by
//! the `#[cfg(feature = "live")]` + `#[ignore]`d `sustained_load` test — it
//! never runs in CI / this sandbox (booting a real node is heavy).
//!
//! The scraper is a hand-rolled HTTP/1.1 GET over `tokio::net::TcpStream`,
//! reusing the differential harness's "no HTTP-client crate" approach (the "no
//! second crate for a `00 §4` job" rule). It only handles exactly the request a
//! local tmpnet node answers on `/ext/metrics`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};

use crate::metrics::Exposition;

/// Errors raised bringing up / scraping the load-test node.
#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    /// The Rust `avalanchers` binary could not be located.
    #[error("avalanchers binary not found: {0}")]
    RustBinaryMissing(String),
    /// The node process failed to spawn.
    #[error("spawning avalanchers: {0}")]
    Spawn(#[source] std::io::Error),
    /// A transport-level failure talking to `/ext/metrics`.
    #[error("metrics scrape transport: {0}")]
    Transport(#[from] std::io::Error),
    /// The HTTP response was malformed or carried a non-200 status.
    #[error("bad metrics http response: {0}")]
    BadResponse(String),
    /// The scraped exposition failed to parse.
    #[error("parse exposition: {0}")]
    Parse(#[from] crate::metrics::ParseError),
    /// The node did not come up within the deadline.
    #[error("node not ready within {0:?}")]
    NotReady(Duration),
}

/// A live single-node Rust network for the sustained-load arm.
///
/// Owns the child process; dropping it — or [`LoadNode::shutdown`] — kills it.
pub struct LoadNode {
    /// The node's HTTP API base, e.g. `http://127.0.0.1:9650`.
    pub api_base: String,
    work_dir: PathBuf,
    child: Child,
}

/// Base HTTP API port the load node binds.
const HTTP_PORT: u16 = 9650;
/// Base staking port the load node binds.
const STAKING_PORT: u16 = 9651;

impl LoadNode {
    /// Boot one `avalanchers` node for a load run keyed by `seed`.
    ///
    /// **Live path** — non-`cfg`-gated so it always compiles, invoked only by the
    /// gated `sustained_load` test.
    ///
    /// ## LIVE-ARM operator handoff
    /// This sketches a single local node: `--http-port`/`--staking-port`/
    /// `--data-dir`/`--network-id=local`. A nightly operator extends this with the
    /// full single-node genesis + pre-funded key allocation that the
    /// [`crate::generator::LoadGenerator`]'s `from`/`to` account indices map onto
    /// (the same way [`tests/differential/src/network.rs`]'s `spawn_node` defers
    /// genesis + cert wiring). The structure here — binary location, child
    /// ownership/teardown, the readiness poll, and the `/ext/metrics` scrape — is
    /// real and exercised by the offline arms.
    ///
    /// # Errors
    /// Returns [`LiveError`] if the binary is missing or fails to spawn.
    pub fn start(seed: u64) -> Result<LoadNode, LiveError> {
        let bin = locate_rust_binary()?;
        let work_dir = std::env::temp_dir().join(format!("load-net-{seed}"));
        let _ = std::fs::create_dir_all(&work_dir);
        let log_path = work_dir.join("node.log");
        let log = std::fs::File::create(&log_path).map_err(LiveError::Spawn)?;
        let log_err = log.try_clone().map_err(LiveError::Spawn)?;

        let mut cmd = Command::new(&bin);
        cmd.arg(format!("--http-port={HTTP_PORT}"))
            .arg(format!("--staking-port={STAKING_PORT}"))
            .arg(format!("--data-dir={}", work_dir.display()))
            .arg("--network-id=local")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .stdin(Stdio::null())
            .kill_on_drop(true);

        let child = cmd.spawn().map_err(LiveError::Spawn)?;

        Ok(LoadNode {
            api_base: format!("http://127.0.0.1:{HTTP_PORT}"),
            work_dir,
            child,
        })
    }

    /// Poll `/ext/metrics` until it answers (the node is serving its API), or
    /// time out.
    ///
    /// # Errors
    /// Returns [`LiveError::NotReady`] if `/ext/metrics` is not answerable within
    /// `within`.
    pub async fn await_ready(&self, within: Duration) -> Result<(), LiveError> {
        let deadline = tokio::time::Instant::now()
            .checked_add(within)
            .ok_or(LiveError::NotReady(within))?;
        loop {
            if self.scrape_metrics().await.is_ok() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(LiveError::NotReady(within));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Scrape and parse the node's `/ext/metrics` Prometheus exposition.
    ///
    /// # Errors
    /// Returns [`LiveError`] on transport failure, a non-200 status, or a parse
    /// error.
    pub async fn scrape_metrics(&self) -> Result<Exposition, LiveError> {
        let (host, port) = parse_authority(&self.api_base)
            .ok_or_else(|| LiveError::BadResponse(format!("bad api base: {}", self.api_base)))?;
        let body = http_get(&host, port, "/ext/metrics").await?;
        Ok(Exposition::parse(&body)?)
    }

    /// Kill the child and remove the work dir.
    pub async fn shutdown(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

impl Drop for LoadNode {
    fn drop(&mut self) {
        // Best-effort kill on drop so a panicking test never leaks the node.
        let _ = self.child.start_kill();
    }
}

/// GET `path` from `host:port` over a hand-rolled HTTP/1.1 request on a raw
/// `tokio::net::TcpStream` and return the response body (no HTTP-client crate).
///
/// # Errors
/// Returns [`LiveError`] on transport failure, a non-utf8 body, or a non-200
/// status.
async fn http_get(host: &str, port: u16, path: &str) -> Result<String, LiveError> {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: text/plain\r\nConnection: close\r\n\r\n"
    );
    let mut stream = TcpStream::connect((host, port)).await?;
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;

    let text = String::from_utf8(raw)
        .map_err(|e| LiveError::BadResponse(format!("non-utf8 response: {e}")))?;
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| LiveError::BadResponse("no header/body separator".to_owned()))?;
    let status_line = head
        .lines()
        .next()
        .ok_or_else(|| LiveError::BadResponse("empty response".to_owned()))?;
    if !(status_line.contains(" 200 ") || status_line.contains(" 2")) {
        return Err(LiveError::BadResponse(format!(
            "non-200 status: {status_line}"
        )));
    }
    Ok(body.to_owned())
}

/// Split `http://host:port[/...]` into `(host, port)`.
fn parse_authority(api_base: &str) -> Option<(String, u16)> {
    let rest = api_base.strip_prefix("http://")?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = authority.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    Some((host.to_owned(), port))
}

/// Locate the built Rust `avalanchers` binary (honors `$AVALANCHERS_PATH`, else
/// the conventional Cargo target locations). Mirrors the differential harness.
fn locate_rust_binary() -> Result<String, LiveError> {
    if let Ok(path) = std::env::var("AVALANCHERS_PATH") {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
        return Err(LiveError::RustBinaryMissing(path));
    }
    for candidate in [
        "target/release/avalanchers",
        "target/debug/avalanchers",
        "../../target/release/avalanchers",
        "../../target/debug/avalanchers",
    ] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate.to_owned());
        }
    }
    Err(LiveError::RustBinaryMissing(
        "set $AVALANCHERS_PATH or build `avalanchers`".to_owned(),
    ))
}
