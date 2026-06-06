// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-peer TLS transport + identity (Wave B: M2.7–M2.10).
//!
//! Mirrors Go `network/peer/{tls_config,upgrader,ip,ip_signer}.go`. The peer
//! actor / message queue / runtime (M2.11+) is a later wave and not present
//! here.

pub mod ip;
pub mod ip_signer;
pub mod tls_config;
pub mod upgrader;
pub mod verifier;
