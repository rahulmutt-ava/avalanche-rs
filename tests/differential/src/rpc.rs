// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Minimal JSON-RPC-over-HTTP client hand-rolled on `tokio::net::TcpStream`.
//!
//! The workspace deliberately ships no HTTP-client crate (the "no second crate
//! for a `00 §4` job" rule), so the live observation scrape speaks just enough
//! HTTP/1.1 to POST a JSON-RPC envelope and parse a single response. Shared by
//! [`crate::observation::Observation::collect`] and the live tx-driver.

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::observation::ObsError;

/// A parsed `http://host:port` API base.
pub(crate) struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    /// Parse `http://host:port` (the only scheme a local tmpnet node serves).
    pub(crate) fn parse(api_base: &str) -> Result<Endpoint, ObsError> {
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
pub(crate) async fn call(
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

#[cfg(test)]
mod tests {
    use super::Endpoint;

    #[test]
    fn parses_host_and_port_dropping_path() {
        let ep = Endpoint::parse("http://127.0.0.1:9650/ext/info").expect("parse");
        assert_eq!(ep.host, "127.0.0.1");
        assert_eq!(ep.port, 9650);
    }

    #[test]
    fn rejects_non_http_scheme() {
        assert!(Endpoint::parse("https://x:1").is_err(), "https must be rejected");
        assert!(Endpoint::parse("127.0.0.1:9650").is_err(), "missing scheme rejected");
    }
}
