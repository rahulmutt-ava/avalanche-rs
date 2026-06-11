// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The per-crate error enum (specs 12 §3, 00 §8).

/// Errors produced by the API server subsystem.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// A route's base/endpoint failed URL validation (Go's
    /// `url.ParseRequestURI`), or the resulting mount path was malformed.
    #[error("invalid route path {path:?}: {msg}")]
    InvalidPath {
        /// The offending base/endpoint path.
        path: String,
        /// Why it was rejected.
        msg: String,
    },

    /// A route or alias was registered under a path that is already taken
    /// (mirror Go `errAlreadyReserved`).
    #[error("API route {path:?} is already reserved")]
    AlreadyReserved {
        /// The conflicting path.
        path: String,
    },

    /// The HTTP listener could not be bound or accept connections.
    #[error("failed to bind/serve HTTP listener on {addr}: {source}")]
    Listen {
        /// The address the server tried to bind.
        addr: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

/// The crate result alias.
pub type Result<T> = std::result::Result<T, ApiError>;

/// The standard gorilla `json2` / `utils/json` JSON-RPC 2.0 error codes
/// (`gorilla/rpc@v1.2.0/v2/json2/error.go`, 14 §16.1/§16.5).
///
/// Domain errors returned by a service handler default to `SERVER` (`-32000`)
/// with the message set to the error's `to_string()` — gorilla never inspects
/// the error further (14 §16.1). The remaining codes are raised by the
/// transport / dispatch layer itself.
pub mod json2_code {
    /// `E_PARSE`: the request body is not valid JSON (`server.go:107`).
    pub const PARSE: i32 = -32700;
    /// `E_INVALID_REQ`: valid JSON but not a valid JSON-RPC request, or a wrong
    /// `jsonrpc` version (`server.go:113,166`).
    pub const INVALID_REQ: i32 = -32600;
    /// `E_NO_METHOD`: `Service.Method` is not registered; also raised by the
    /// uppercase-method guard (`utils/json/codec.go::errUppercaseMethod`).
    pub const NO_METHOD: i32 = -32601;
    /// `E_BAD_PARAMS`: params present but unmarshal failed (`errInvalidArg`).
    pub const BAD_PARAMS: i32 = -32602;
    /// `E_INTERNAL`: reserved internal error.
    pub const INTERNAL: i32 = -32603;
    /// `E_SERVER`: the **default** for any handler-returned `error` that is not
    /// already a `*json2.Error` (`server.go:191`).
    pub const SERVER: i32 = -32000;
}

/// A JSON-RPC 2.0 error object as it appears on the wire (gorilla `json2`
/// shape, 14 §16.5): `{ "code": <i32>, "message": "...", "data": <null|any> }`.
///
/// `data` is always serialized — even as an explicit `null` — to mirror
/// `json2`, which emits the field unconditionally.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JsonRpcError {
    /// The json2 error code (see [`json2_code`]).
    pub code: i32,
    /// The human-readable message. For a domain error this is the originating
    /// error's `to_string()`, preserved byte-for-byte (14 §16.5 byte-stability).
    pub message: String,
    /// Optional structured data. gorilla emits an explicit `null` here, so the
    /// field is never skipped on serialization.
    pub data: Option<serde_json::Value>,
}

/// Boundary trait converting a domain error into the on-wire [`JsonRpcError`].
///
/// The blanket impl mirrors gorilla exactly: any [`std::error::Error`] becomes
/// `{ code: -32000, message: err.to_string(), data: null }` (14 §16.5). A
/// service whose error needs a different code (rare in avalanchego) can provide
/// its own impl.
pub trait IntoJsonRpcError {
    /// Produces the on-wire JSON-RPC error object for this domain error.
    fn to_json_rpc(&self) -> JsonRpcError;
}

impl<E: std::error::Error> IntoJsonRpcError for E {
    fn to_json_rpc(&self) -> JsonRpcError {
        JsonRpcError {
            // -32000, exactly like json2 `WriteError` for a non-`*json2.Error`.
            code: json2_code::SERVER,
            // BYTE-STABLE: must equal the Go `err.Error()` string.
            message: self.to_string(),
            // json2 emits an explicit `null`.
            data: None,
        }
    }
}
