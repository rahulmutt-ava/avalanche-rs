// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-network` crate error enum.
//!
//! Defined locally (per `specs/05` §8): `ava-network` does NOT depend on
//! `ava-message`, so it carries its own `thiserror` enum preserving the Go
//! sentinel errors from `network/peer/tls_config.go`, `upgrader.go`, and
//! `ip.go` as typed variants.

/// Result alias for `ava-network`.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors produced by the TLS transport + identity layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No certificates were presented by the peer during the TLS handshake.
    /// Mirrors Go `ErrNoCertsSent`.
    #[error("no certificates sent by peer")]
    NoCertsSent,

    /// The peer presented an empty leaf certificate. Mirrors Go `ErrEmptyCert`.
    #[error("certificate sent by peer is empty")]
    EmptyCert,

    /// An ECDSA leaf key used a curve other than P-256. Mirrors Go
    /// `ErrCurveMismatch` ("only P256 is allowed for ECDSA").
    #[error("only P256 is allowed for ECDSA")]
    CurveMismatch,

    /// The leaf key used an unsupported algorithm (neither P-256 ECDSA nor a
    /// well-formed RSA key). Mirrors Go `ErrUnsupportedKeyType`.
    #[error("key type is not supported")]
    UnsupportedKeyType,

    /// A signed IP's timestamp is further in the future than `now + 60s`.
    /// Mirrors Go `errTimestampTooFarInFuture`.
    #[error("timestamp too far in the future")]
    TimestampTooFarInFuture,

    /// The TLS signature over the signed IP did not verify against the peer's
    /// certificate. Mirrors Go `errInvalidTLSSignature`.
    #[error("invalid TLS signature")]
    InvalidTlsSignature,

    /// The TLS handshake finished with no peer certificate. Mirrors Go
    /// `errNoCert` in `upgrader.go`.
    #[error("tls handshake finished with no peer certificate")]
    NoPeerCertificate,

    /// A leaf certificate failed the strict staking parser. Wraps the
    /// `ava-crypto` error string.
    #[error("certificate parse failed: {0}")]
    CertificateParse(String),

    /// Building a `rustls` TLS configuration failed.
    #[error("tls config error: {0}")]
    TlsConfig(String),

    /// A wrapped `rustls` error surfaced during the handshake.
    #[error("tls error: {0}")]
    Tls(String),

    /// Generating or loading the staking identity failed.
    #[error("identity error: {0}")]
    Identity(String),

    /// Signing a value with the local staking / BLS key failed.
    #[error("signing failed: {0}")]
    Signing(String),

    /// A low-level I/O error during the TCP/TLS upgrade.
    #[error("io error: {0}")]
    Io(String),

    /// No router was available to map ports (the no-op `NoRouter` was asked to
    /// map a port). Mirrors Go `errNoRouterCantMapPorts`.
    #[error("can't map ports without a known router")]
    NoRouter,

    /// A NAT traversal operation (UPnP / NAT-PMP map / unmap / external-IP)
    /// failed. Carries the underlying gateway error string.
    #[error("nat error: {0}")]
    Nat(String),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl From<ava_crypto::Error> for Error {
    fn from(e: ava_crypto::Error) -> Self {
        Error::CertificateParse(e.to_string())
    }
}
