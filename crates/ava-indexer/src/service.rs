// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `index.*` JSON-RPC service (Go `indexer/service.go`; spec 14 §7).
//!
//! Six methods, mounted per index at `/ext/index/<chainAlias>/{block,tx,vtx}`
//! through the gorilla-parity shim (`ava_api::jsonrpc`):
//! `getLastAccepted`, `getContainerByIndex`, `getContainerByID`,
//! `getContainerRange` (capped at 1024), `getIndex`, `isAccepted`.
//!
//! Reply/argument JSON mirrors Go field-for-field: `ids.ID` as cb58 strings,
//! `json.Uint64` as quoted decimal strings (accepting bare numbers too, as
//! Go's `UnmarshalJSON` does), `formatting.Encoding` as `"hex"`/`"hexnc"`/
//! `"hexc"`/`"json"`, and `time.Time` as RFC3339Nano (rendered in UTC; Go
//! renders in the node's local zone, which is UTC on production nodes).

use std::sync::Arc;

use ava_api::{BoxedHandler, RpcError, ServiceRegistry, rpc_service};
use axum::Router;
use axum::routing::post;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use ava_crypto::formatting;
use ava_types::id::Id;

use crate::container::Container;
use crate::error::Error;
use crate::index::IndexReader;

// ---------------------------------------------------------------------------
// `avajson` — Go `utils/json` numeric encodings
// ---------------------------------------------------------------------------

/// avalanchego `utils/json` numeric encoding: `json.Uint64` marshals as a
/// quoted decimal string and unmarshals from either a quoted string or a bare
/// number (Go strips optional quotes before parsing).
pub mod avajson {
    use serde::de::Visitor;
    use serde::{Deserializer, Serializer};

    /// Serialize a `u64` as a quoted decimal string (`json.Uint64`).
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize_u64<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    /// Deserialize a `u64` from a quoted decimal string or bare number.
    ///
    /// # Errors
    /// Returns a deserialization error for anything else.
    pub fn deserialize_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        struct U64Visitor;

        impl Visitor<'_> for U64Visitor {
            type Value = u64;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a u64 as a quoted decimal string or number")
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<u64, E> {
                Ok(v)
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<u64, E> {
                v.parse::<u64>().map_err(E::custom)
            }
        }

        d.deserialize_any(U64Visitor)
    }
}

// ---------------------------------------------------------------------------
// Encoding (Go `utils/formatting/encoding.go` JSON forms)
// ---------------------------------------------------------------------------

/// Go `formatting.Encoding`'s JSON identity: `"hex"` (default; checksummed),
/// `"hexnc"`, `"hexc"`, `"json"`. Unmarshaling is case-insensitive; `json` is
/// rejected on the byte-encode path with Go's exact message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encoding {
    /// Checksummed hex (`0x…` + 4-byte sha256 tail) — the zero value.
    #[default]
    Hex,
    /// Hex without a checksum.
    HexNc,
    /// Checksummed hex (same wire form as [`Encoding::Hex`]).
    HexC,
    /// JSON — unsupported for byte payloads (Go errors).
    Json,
}

impl Encoding {
    /// The Go `String()` (and JSON) rendering.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Encoding::Hex => "hex",
            Encoding::HexNc => "hexnc",
            Encoding::HexC => "hexc",
            Encoding::Json => "json",
        }
    }

    /// Encodes `bytes` per this encoding (Go `formatting.Encode`).
    fn encode(self, bytes: &[u8]) -> Result<String, RpcError> {
        let enc = match self {
            Encoding::Hex => formatting::Encoding::Hex,
            Encoding::HexNc => formatting::Encoding::HexNc,
            Encoding::HexC => formatting::Encoding::HexC,
            // Go: errUnsupportedEncodingInMethod.
            Encoding::Json => {
                return Err(RpcError::server("unsupported encoding in method"));
            }
        };
        formatting::encode(enc, bytes).map_err(|e| RpcError::server(e.to_string()))
    }
}

impl Serialize for Encoding {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Encoding {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        // Go lowercases the raw token before matching.
        match raw.to_ascii_lowercase().as_str() {
            "hex" => Ok(Encoding::Hex),
            "hexnc" => Ok(Encoding::HexNc),
            "hexc" => Ok(Encoding::HexC),
            "json" => Ok(Encoding::Json),
            _ => Err(serde::de::Error::custom("invalid encoding")),
        }
    }
}

// ---------------------------------------------------------------------------
// Timestamp (Go `time.Time` JSON marshaling = RFC3339Nano)
// ---------------------------------------------------------------------------

/// Renders Unix nanoseconds as Go's `time.Time` JSON form (RFC3339Nano, UTC):
/// trailing zeros of the fractional second are trimmed and the dot dropped
/// for whole seconds.
pub(crate) fn format_timestamp(nanos: i64) -> String {
    let secs = nanos.div_euclid(1_000_000_000);
    let sub = u32::try_from(nanos.rem_euclid(1_000_000_000)).unwrap_or(0);
    let dt = DateTime::<Utc>::from_timestamp(secs, sub).unwrap_or(DateTime::UNIX_EPOCH);
    let mut out = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
    if sub != 0 {
        let mut frac = format!("{sub:09}");
        while frac.ends_with('0') {
            frac.pop();
        }
        out.push('.');
        out.push_str(&frac);
    }
    out.push('Z');
    out
}

// ---------------------------------------------------------------------------
// Wire types (Go `indexer/service.go`)
// ---------------------------------------------------------------------------

/// Go `FormattedContainer`: the reply shape shared by four of the methods.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FormattedContainer {
    /// The container id (cb58).
    pub id: Id,
    /// The container bytes in the requested encoding.
    pub bytes: String,
    /// Acceptance time (RFC3339Nano, from the indexer's clock at accept).
    pub timestamp: String,
    /// The encoding of `bytes`.
    pub encoding: Encoding,
    /// The acceptance index (`json.Uint64`).
    #[serde(serialize_with = "avajson::serialize_u64")]
    pub index: u64,
}

impl FormattedContainer {
    /// Go `newFormattedContainer`.
    fn new(c: &Container, index: u64, encoding: Encoding) -> Result<Self, RpcError> {
        Ok(Self {
            id: c.id,
            bytes: encoding.encode(&c.bytes)?,
            timestamp: format_timestamp(c.timestamp),
            encoding,
            index,
        })
    }
}

/// Args for `index.getLastAccepted`.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct GetLastAcceptedArgs {
    /// The requested byte encoding.
    #[serde(default)]
    pub encoding: Encoding,
}

/// Args for `index.getContainerByIndex`.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct GetContainerByIndexArgs {
    /// The acceptance index to fetch.
    #[serde(default, deserialize_with = "avajson::deserialize_u64")]
    pub index: u64,
    /// The requested byte encoding.
    #[serde(default)]
    pub encoding: Encoding,
}

/// Args for `index.getContainerRange`.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct GetContainerRangeArgs {
    /// First index of the window.
    #[serde(
        default,
        rename = "startIndex",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub start_index: u64,
    /// Window length, in `[1, 1024]`.
    #[serde(
        default,
        rename = "numToFetch",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub num_to_fetch: u64,
    /// The requested byte encoding.
    #[serde(default)]
    pub encoding: Encoding,
}

/// Reply for `index.getContainerRange`.
#[derive(Clone, Debug, Serialize)]
pub struct GetContainerRangeResponse {
    /// The fetched window, in acceptance order.
    pub containers: Vec<FormattedContainer>,
}

/// Args for `index.getIndex`.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct GetIndexArgs {
    /// The container id to look up.
    #[serde(default)]
    pub id: Id,
}

/// Reply for `index.getIndex`.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct GetIndexResponse {
    /// The acceptance index (`json.Uint64`).
    #[serde(serialize_with = "avajson::serialize_u64")]
    pub index: u64,
}

/// Args for `index.isAccepted`.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct IsAcceptedArgs {
    /// The container id to look up.
    #[serde(default)]
    pub id: Id,
}

/// Reply for `index.isAccepted`.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct IsAcceptedResponse {
    /// Whether the container is indexed as accepted.
    #[serde(rename = "isAccepted")]
    pub is_accepted: bool,
}

/// Args for `index.getContainerByID`.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct GetContainerByIdArgs {
    /// The container id to fetch.
    #[serde(default)]
    pub id: Id,
    /// The requested byte encoding.
    #[serde(default)]
    pub encoding: Encoding,
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

impl From<Error> for RpcError {
    /// The gorilla default: any handler error is a `-32000` whose message is
    /// the error string (14 §16.1).
    fn from(e: Error) -> Self {
        RpcError::server(e.to_string())
    }
}

/// One mounted `index.*` service over one [`IndexReader`]
/// (Go `indexer.service`).
pub struct IndexService {
    index: Arc<dyn IndexReader>,
}

impl IndexService {
    /// A service reading from `index`.
    #[must_use]
    pub fn new(index: Arc<dyn IndexReader>) -> Self {
        Self { index }
    }

    /// Looks up `id`'s index, wrapping the failure like Go's
    /// `"couldn't get index: %w"`.
    fn index_of(&self, id: &Id) -> Result<u64, RpcError> {
        self.index
            .get_index(id)
            .map_err(|e| RpcError::server(format!("couldn't get index: {e}")))
    }
}

#[rpc_service("index")]
impl IndexService {
    /// Go `service.GetLastAccepted`.
    ///
    /// # Errors
    /// `-32000` with Go's error strings (none accepted / encode failure).
    pub async fn get_last_accepted(
        &self,
        args: GetLastAcceptedArgs,
    ) -> Result<FormattedContainer, RpcError> {
        let container = self.index.get_last_accepted()?;
        let index = self.index_of(&container.id)?;
        FormattedContainer::new(&container, index, args.encoding)
    }

    /// Go `service.GetContainerByIndex`.
    ///
    /// # Errors
    /// `-32000` with Go's error strings (no container at index / encode).
    pub async fn get_container_by_index(
        &self,
        args: GetContainerByIndexArgs,
    ) -> Result<FormattedContainer, RpcError> {
        let container = self.index.get_container_by_index(args.index)?;
        let index = self.index_of(&container.id)?;
        FormattedContainer::new(&container, index, args.encoding)
    }

    /// Go `service.GetContainerRange` — at most 1024 containers; a window
    /// reaching past the end returns what exists.
    ///
    /// # Errors
    /// `-32000` with Go's error strings (cap / bounds / encode).
    pub async fn get_container_range(
        &self,
        args: GetContainerRangeArgs,
    ) -> Result<GetContainerRangeResponse, RpcError> {
        let fetched = self
            .index
            .get_container_range(args.start_index, args.num_to_fetch)?;
        let mut containers = Vec::with_capacity(fetched.len());
        for container in &fetched {
            let index = self.index_of(&container.id)?;
            containers.push(FormattedContainer::new(container, index, args.encoding)?);
        }
        Ok(GetContainerRangeResponse { containers })
    }

    /// Go `service.GetIndex`.
    ///
    /// # Errors
    /// `-32000` `not found` for an unknown id.
    pub async fn get_index(&self, args: GetIndexArgs) -> Result<GetIndexResponse, RpcError> {
        let index = self.index.get_index(&args.id)?;
        Ok(GetIndexResponse { index })
    }

    /// Go `service.IsAccepted` — an unknown id is `false`, not an error.
    ///
    /// # Errors
    /// `-32000` only for an unexpected database failure.
    pub async fn is_accepted(&self, args: IsAcceptedArgs) -> Result<IsAcceptedResponse, RpcError> {
        match self.index.get_index(&args.id) {
            Ok(_) => Ok(IsAcceptedResponse { is_accepted: true }),
            Err(Error::Database(ava_database::Error::NotFound)) => {
                Ok(IsAcceptedResponse { is_accepted: false })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Go `service.GetContainerByID`. The Go wire name carries the `ID`
    /// acronym, which pascalize cannot derive — hence the override.
    ///
    /// # Errors
    /// `-32000` `not found` for an unknown id.
    #[rpc(name = "GetContainerByID")]
    pub async fn get_container_by_id(
        &self,
        args: GetContainerByIdArgs,
    ) -> Result<FormattedContainer, RpcError> {
        let container = self.index.get_container_by_id(&args.id)?;
        let index = self.index_of(&container.id)?;
        FormattedContainer::new(&container, index, args.encoding)
    }
}

/// Builds the mounted handler for one index: a single-route axum router
/// dispatching `index.*` through the gorilla shim. The indexer hands this to
/// the [`crate::PathAdder`] for `/ext/index/<chainAlias>/<kind>` (14 §7).
pub fn index_handler(index: Arc<dyn IndexReader>) -> BoxedHandler {
    let mut registry = ServiceRegistry::new();
    Arc::new(IndexService::new(index)).register_rpc(&mut registry);
    Router::new()
        .route("/", post(ava_api::dispatch))
        .with_state(Arc::new(registry))
}

#[cfg(test)]
// Tests index into fixtures and `serde_json::Value` replies and do plain
// test-fixture arithmetic (`UNIX_EPOCH + ...`), both idiomatic in tests
// (precedent: ava-api jsonrpc tests).
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    use ava_api::ServiceRegistry;
    use ava_database::memdb::MemDb;
    use ava_types::id::Id;
    use ava_utils::clock::MockClock;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::index::{Index, IndexReader};

    fn populated_index(n: u8) -> Arc<Index<MemDb>> {
        // 2023-11-14T22:13:20.123456789Z — a nanosecond fraction with no
        // trailing zeros, to exercise RFC3339Nano rendering.
        let clock = Arc::new(MockClock::at(
            UNIX_EPOCH + Duration::from_nanos(1_700_000_000_123_456_789),
        ));
        let index = Index::new(Arc::new(MemDb::new()), clock).expect("Index::new()");
        for i in 0..n {
            index
                .accept(Id::from([i.wrapping_add(1); 32]), &[i, i, i])
                .expect("Index::accept()");
        }
        Arc::new(index)
    }

    fn registry(index: Arc<dyn IndexReader>) -> ServiceRegistry {
        let mut reg = ServiceRegistry::new();
        Arc::new(IndexService::new(index)).register_rpc(&mut reg);
        reg
    }

    async fn call(index: Arc<dyn IndexReader>, body: Value) -> Value {
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let response = index_handler(index)
            .oneshot(request)
            .await
            .expect("oneshot");
        assert_eq!(StatusCode::OK, response.status(), "JSON-RPC always 200");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    // ------------------------------------------------------------------
    // Red (M8.24): the 6 methods of 14 §7 are registered under their exact
    // Go wire names — incl. the acronym `GetContainerByID`, which the macro's
    // pascalize cannot derive from snake_case.
    // ------------------------------------------------------------------
    #[test]
    fn index_method_set() {
        let reg = registry(populated_index(1));
        assert_eq!(6, reg.len(), "exactly 6 index.* methods");
        for method in [
            "GetLastAccepted",
            "GetContainerByIndex",
            "GetContainerByID",
            "GetContainerRange",
            "GetIndex",
            "IsAccepted",
        ] {
            assert!(
                reg.lookup("index", method).is_some(),
                "index.{method} registered"
            );
        }
        // The pascalize-derived spelling must NOT be registered: dispatch
        // matches the remainder exactly (getContainerByID vs getContainerById).
        assert!(
            reg.lookup("index", "GetContainerById").is_none(),
            "acronym method must be name-overridden"
        );
    }

    // FormattedContainer is the Go reply shape: cb58 id, checksummed-hex
    // bytes, RFC3339Nano timestamp, encoding echo, quoted-decimal index.
    #[tokio::test]
    async fn get_last_accepted_reply_shape() {
        let index = populated_index(3);
        let body = call(
            index,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "index.getLastAccepted",
                "params": [{"encoding": "hex"}],
            }),
        )
        .await;
        let result = &body["result"];
        assert_eq!(
            Id::from([3; 32]).to_string(),
            result["id"].as_str().expect("id string"),
            "last accepted id (cb58)"
        );
        // formatting.Hex appends a 4-byte sha256 checksum then hex-encodes.
        let expected_bytes =
            ava_crypto::formatting::encode(ava_crypto::formatting::Encoding::Hex, &[2, 2, 2])
                .expect("formatting::encode()");
        assert_eq!(expected_bytes, result["bytes"], "checksummed hex bytes");
        assert_eq!(
            "2023-11-14T22:13:20.123456789Z", result["timestamp"],
            "RFC3339Nano timestamp"
        );
        assert_eq!("hex", result["encoding"], "encoding echo");
        assert_eq!("2", result["index"], "json.Uint64 quoted index");
    }

    // getContainerRange is capped at 1024 and surfaces Go's exact error
    // string; valid windows return {containers: [FormattedContainer]}.
    #[tokio::test]
    async fn get_container_range_cap() {
        let index = populated_index(3);
        let body = call(
            Arc::clone(&index) as Arc<dyn IndexReader>,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "index.getContainerRange",
                "params": [{"startIndex": 0, "numToFetch": 1025}],
            }),
        )
        .await;
        assert_eq!(-32000, body["error"]["code"], "domain error code");
        assert_eq!(
            "numToFetch must be in [1,1024] but is 1025", body["error"]["message"],
            "Go-byte-stable cap message"
        );

        let body = call(
            index,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "index.getContainerRange",
                "params": [{"startIndex": "1", "numToFetch": "2", "encoding": "hex"}],
            }),
        )
        .await;
        let containers = body["result"]["containers"]
            .as_array()
            .expect("containers array");
        assert_eq!(2, containers.len(), "range window length");
        assert_eq!("1", containers[0]["index"], "first index in window");
        assert_eq!("2", containers[1]["index"], "second index in window");
    }

    // getIndex / isAccepted / getContainerByID round-trip; the unknown-id
    // paths match Go (`not found` / isAccepted=false).
    #[tokio::test]
    async fn id_lookups() {
        let index = populated_index(2);
        let known = Id::from([1; 32]).to_string();
        let unknown = Id::from([9; 32]).to_string();

        let body = call(
            Arc::clone(&index) as Arc<dyn IndexReader>,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "index.getIndex",
                "params": [{"id": known}],
            }),
        )
        .await;
        assert_eq!("0", body["result"]["index"], "getIndex known id");

        let body = call(
            Arc::clone(&index) as Arc<dyn IndexReader>,
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "index.getIndex",
                "params": [{"id": unknown}],
            }),
        )
        .await;
        assert_eq!("not found", body["error"]["message"], "getIndex unknown id");

        let body = call(
            Arc::clone(&index) as Arc<dyn IndexReader>,
            json!({
                "jsonrpc": "2.0", "id": 3, "method": "index.isAccepted",
                "params": [{"id": known}],
            }),
        )
        .await;
        assert_eq!(
            json!(true),
            body["result"]["isAccepted"],
            "isAccepted known"
        );

        let body = call(
            Arc::clone(&index) as Arc<dyn IndexReader>,
            json!({
                "jsonrpc": "2.0", "id": 4, "method": "index.isAccepted",
                "params": [{"id": unknown}],
            }),
        )
        .await;
        assert_eq!(
            json!(false),
            body["result"]["isAccepted"],
            "isAccepted unknown"
        );

        let body = call(
            index,
            json!({
                "jsonrpc": "2.0", "id": 5, "method": "index.getContainerByID",
                "params": [{"id": known, "encoding": "hex"}],
            }),
        )
        .await;
        assert_eq!(known, body["result"]["id"], "getContainerByID id echo");
    }

    // Go `formatting.Encoding` JSON forms: hex/hexnc/hexc accepted
    // case-insensitively, json rejected on this path, anything else invalid.
    #[tokio::test]
    async fn encoding_forms() {
        let index = populated_index(1);
        for (enc, ok) in [
            ("hex", true),
            ("HEX", true),
            ("hexnc", true),
            ("hexc", true),
            ("json", false),
            ("cb58", false),
        ] {
            let body = call(
                Arc::clone(&index) as Arc<dyn IndexReader>,
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "index.getLastAccepted",
                    "params": [{"encoding": enc}],
                }),
            )
            .await;
            if ok {
                assert!(
                    body.get("error").is_none(),
                    "encoding {enc} accepted: {body}"
                );
            } else {
                assert!(body.get("error").is_some(), "encoding {enc} rejected");
            }
        }
    }

    // RFC3339Nano trimming mirrors Go's time.Time JSON marshaling: trailing
    // fractional zeros trimmed, whole seconds drop the dot, UTC "Z" suffix.
    #[test]
    fn timestamp_rfc3339_nano_trimming() {
        for (nanos, want) in [
            (1_700_000_000_000_000_000_i64, "2023-11-14T22:13:20Z"),
            (1_700_000_000_500_000_000, "2023-11-14T22:13:20.5Z"),
            (1_700_000_000_123_456_789, "2023-11-14T22:13:20.123456789Z"),
            (0, "1970-01-01T00:00:00Z"),
        ] {
            assert_eq!(want, format_timestamp(nanos), "format_timestamp({nanos})");
        }
    }
}
