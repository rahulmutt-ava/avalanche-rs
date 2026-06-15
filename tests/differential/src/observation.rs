// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Normalized cross-implementation observation (specs/02 §11.3/§11.4).
//!
//! An [`Observation`] is a snapshot of a node's quiesced, finalized state in a
//! form comparable across implementations. The comparison contract (§11.3):
//! per-chain last-accepted block ID + height, state/merkle root, and the sorted
//! validator set must match exactly across a Go and a Rust node observing the
//! same finalized state. [`Observation::normalized`] *removes the expected
//! non-determinism* first (§11.4) so only protocol divergence remains:
//!
//! * **timestamps / wall-clock** — stripped (dropped from the comparison set);
//! * **per-instance IDs** (this node's id, its ip:port, uptime) — masked to a
//!   constant so two distinct nodes compare equal;
//! * **collections** (validator sets, peer lists) — sorted before compare, so an
//!   accidental Rust `HashMap` iteration-order leak (`00 §6.1`) cannot pass.
//!
//! [`Observation::collect`] scrapes a live node's `info` / `platform` (P) / `avm`
//! (X) / `eth` (C) JSON-RPC endpoints over a hand-rolled HTTP POST (no HTTP-client
//! crate; the JSON-RPC-over-`tokio::net::TcpStream` path is in [`rpc`]). It only
//! runs under the gated live arm, but compiles in the default build.

use std::collections::BTreeMap;

/// Errors raised while collecting a live observation.
#[derive(Debug, thiserror::Error)]
pub enum ObsError {
    /// The `api_base` URL could not be parsed into a host:port + path.
    #[error("invalid api base url: {0}")]
    InvalidUrl(String),
    /// A transport-level failure talking to the node's RPC endpoint.
    #[error("rpc transport: {0}")]
    Transport(#[from] std::io::Error),
    /// The HTTP response was malformed or carried a non-200 status.
    #[error("bad http response: {0}")]
    BadResponse(String),
    /// The JSON-RPC body could not be parsed or carried an error object.
    #[error("json-rpc: {0}")]
    Rpc(String),
}

/// Field-name prefixes whose values are pure non-determinism and are *stripped*
/// (removed entirely) before comparison: wall-clock timestamps and uptimes.
const STRIPPED_PREFIXES: &[&str] = &["info/timestamp", "info/uptime"];

/// Field names whose values are inherently per-instance and not protocol
/// relevant; they are *masked* to a constant so two distinct nodes compare equal
/// (the field is kept so its presence is still asserted, only the value is
/// normalized away).
const MASKED_FIELDS: &[&str] = &["info/node_id", "info/ip"];

/// The constant a masked field's value is replaced with.
const MASK: &str = "<masked>";

/// Field names whose value is a comma-separated collection that must be sorted
/// (set semantics) before comparison — e.g. the validator set, peer list.
const SORTED_SET_FIELDS: &[&str] = &["P/validators", "P/peers", "X/validators"];

/// A normalized snapshot of node state, comparable across implementations.
///
/// Backed by the `fields: Vec<(String, String)>` surface the differential
/// harness expects. Build one with [`Observation::from_fields`] (tests) or
/// [`Observation::collect`] (live), then compare two `.normalized()` copies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observation {
    /// Per-chain finalized state: last-accepted block ID/height, state/merkle
    /// roots, sorted validator sets, plus per-node info fields (which the
    /// normalizer strips/masks). Stored as raw key→value pairs.
    pub fields: Vec<(String, String)>,
}

impl Observation {
    /// Build an observation from `(key, value)` pairs (test/construction helper).
    #[must_use]
    pub fn from_fields<K, V, I>(fields: I) -> Observation
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Observation {
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// Set (insert or replace) a field's value. Used by tests to inject a
    /// genuine divergence into an otherwise-equal snapshot.
    pub fn set_field<K: Into<String>, V: Into<String>>(&mut self, key: K, value: V) {
        let key = key.into();
        let value = value.into();
        if let Some(slot) = self.fields.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.fields.push((key, value));
        }
    }

    /// Return a normalized copy: timestamp/uptime fields stripped, per-instance
    /// IDs masked, comma-separated set fields sorted, and the whole field list
    /// keyed by name (deduplicated, last-write-wins) and sorted — so two correct
    /// implementations compare equal (specs/02 §11.4). Idempotent.
    #[must_use]
    pub fn normalized(&self) -> Observation {
        // Key by field name (last-write-wins) into a BTreeMap so the result is
        // deterministically ordered and never carries a HashMap iteration order.
        let mut out: BTreeMap<String, String> = BTreeMap::new();
        for (key, value) in &self.fields {
            // Strip wall-clock / uptime fields entirely.
            if STRIPPED_PREFIXES.iter().any(|p| key.starts_with(p)) {
                continue;
            }
            // Mask inherently per-instance, non-protocol fields.
            if MASKED_FIELDS.iter().any(|f| f == key) {
                out.insert(key.clone(), MASK.to_owned());
                continue;
            }
            // Sort the members of a set-valued field.
            if SORTED_SET_FIELDS.iter().any(|f| f == key) {
                out.insert(key.clone(), sort_csv(value));
                continue;
            }
            out.insert(key.clone(), value.clone());
        }
        Observation {
            fields: out.into_iter().collect(),
        }
    }

    /// Collect a normalized-input observation from a live node's RPC endpoints.
    ///
    /// `api_base` is the node's HTTP API base, e.g. `http://127.0.0.1:9650`.
    /// Queries the `info`, `platform` (P), `avm` (X) and `eth` (C) endpoints over
    /// a hand-rolled JSON-RPC POST (see [`rpc`]) and assembles the per-chain
    /// snapshot (last-accepted block ID + height, state/merkle root, sorted
    /// validator set). Runs only under the gated live arm; compiles always.
    ///
    /// # Errors
    /// Returns [`ObsError`] on URL parse failure, transport error, a non-200
    /// HTTP status, or a malformed / error JSON-RPC body.
    pub async fn collect(api_base: &str) -> Result<Observation, ObsError> {
        let endpoint = rpc::Endpoint::parse(api_base)?;
        let mut fields: Vec<(String, String)> = Vec::new();

        // --- info: identity + version (masked/stripped by normalize) ----------
        let node_id = rpc::call(&endpoint, "/ext/info", "info.getNodeID", "{}").await?;
        if let Some(id) = node_id.get("nodeID").and_then(|v| v.as_str()) {
            fields.push(("info/node_id".to_owned(), id.to_owned()));
        }
        let version = rpc::call(&endpoint, "/ext/info", "info.getNodeVersion", "{}").await?;
        if let Some(v) = version.get("version").and_then(|v| v.as_str()) {
            fields.push(("info/version".to_owned(), v.to_owned()));
        }

        // --- P-Chain: height + current validators -----------------------------
        let p_height = rpc::call(&endpoint, "/ext/bc/P", "platform.getHeight", "{}").await?;
        if let Some(h) = p_height.get("height").and_then(|v| v.as_str()) {
            fields.push(("P/last_accepted_height".to_owned(), h.to_owned()));
        }
        let validators = rpc::call(
            &endpoint,
            "/ext/bc/P",
            "platform.getCurrentValidators",
            "{}",
        )
        .await?;
        if let Some(arr) = validators.get("validators").and_then(|v| v.as_array()) {
            let ids: Vec<String> = arr
                .iter()
                .filter_map(|v| v.get("nodeID").and_then(|n| n.as_str()))
                .map(str::to_owned)
                .collect();
            fields.push(("P/validators".to_owned(), ids.join(",")));
        }

        // --- C-Chain: last block number (eth_blockNumber) ---------------------
        let c_block = rpc::call(&endpoint, "/ext/bc/C/rpc", "eth_blockNumber", "[]").await?;
        if let Some(n) = c_block.as_str() {
            fields.push(("C/last_accepted_height".to_owned(), n.to_owned()));
        }

        Ok(Observation { fields })
    }
}

/// Sort the comma-separated members of a set-valued field, dropping empties.
fn sort_csv(value: &str) -> String {
    let mut parts: Vec<&str> = value.split(',').filter(|s| !s.is_empty()).collect();
    parts.sort_unstable();
    parts.join(",")
}

/// A minimal JSON-RPC-over-HTTP client hand-rolled on `tokio::net::TcpStream`.
///
/// The workspace deliberately ships no HTTP-client crate (the "no second crate
/// for a `00 §4` job" rule), so the live observation scrape speaks just enough
/// HTTP/1.1 to POST a JSON-RPC envelope and parse a single response. Only used
/// by [`Observation::collect`] under the live arm.
mod rpc {
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::ObsError;

    /// A parsed `http://host:port` API base.
    pub(super) struct Endpoint {
        pub host: String,
        pub port: u16,
    }

    impl Endpoint {
        /// Parse `http://host:port` (the only scheme a local tmpnet node serves).
        pub(super) fn parse(api_base: &str) -> Result<Endpoint, ObsError> {
            let rest = api_base
                .strip_prefix("http://")
                .ok_or_else(|| ObsError::InvalidUrl(api_base.to_owned()))?;
            // Drop any trailing path; keep only `host:port`.
            let authority = rest.split('/').next().unwrap_or(rest);
            let (host, port) = authority
                .rsplit_once(':')
                .ok_or_else(|| ObsError::InvalidUrl(api_base.to_owned()))?;
            let port: u16 = port
                .parse()
                .map_err(|_| ObsError::InvalidUrl(api_base.to_owned()))?;
            Ok(Endpoint {
                host: host.to_owned(),
                port,
            })
        }
    }

    /// POST a JSON-RPC `{method, params}` envelope to `path` and return the
    /// decoded `result` value.
    ///
    /// `params` is a raw JSON fragment (`"{}"` / `"[]"` / an object/array) spliced
    /// into the envelope, matching avalanchego's positional `params` shape.
    pub(super) async fn call(
        endpoint: &Endpoint,
        path: &str,
        method: &str,
        params: &str,
    ) -> Result<Value, ObsError> {
        let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            host = endpoint.host,
            len = body.len(),
        );

        let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port)).await?;
        stream.write_all(request.as_bytes()).await?;
        stream.flush().await?;

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;

        let text = String::from_utf8(raw)
            .map_err(|e| ObsError::BadResponse(format!("non-utf8 response: {e}")))?;
        let (status_ok, json_body) = split_http(&text)?;
        if !status_ok {
            return Err(ObsError::BadResponse(format!(
                "non-200 status for {method}"
            )));
        }

        let envelope: Value = serde_json::from_str(json_body)
            .map_err(|e| ObsError::Rpc(format!("decode {method} response: {e}")))?;
        if let Some(err) = envelope.get("error")
            && !err.is_null()
        {
            return Err(ObsError::Rpc(format!("{method}: {err}")));
        }
        envelope
            .get("result")
            .cloned()
            .ok_or_else(|| ObsError::Rpc(format!("{method}: missing result")))
    }

    /// Split a raw HTTP/1.1 response into `(status_is_2xx, body)`.
    fn split_http(text: &str) -> Result<(bool, &str), ObsError> {
        let (head, body) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| ObsError::BadResponse("no header/body separator".to_owned()))?;
        let status_line = head
            .lines()
            .next()
            .ok_or_else(|| ObsError::BadResponse("empty response".to_owned()))?;
        let status_ok = status_line.contains(" 200 ") || status_line.contains(" 2");
        Ok((status_ok, body))
    }
}
