// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-peer TLS transport + identity + the peer actor.
//!
//! Mirrors Go `network/peer/{tls_config,upgrader,ip,ip_signer,peer,
//! message_queue}.go`. Wave B (M2.7–M2.10) provides the TLS + identity
//! foundation; Wave C (M2.14+) adds the three-task [`peer::Peer`] actor, its
//! [`handle::PeerHandle`] control surface, and the handshake / ping-pong
//! handling.

pub mod handle;
pub mod handshake;
pub mod ip;
pub mod ip_signer;
pub mod message_queue;
#[allow(clippy::module_inception)]
pub mod peer;
pub mod testutil;
pub mod tls_config;
pub mod upgrader;
pub mod verifier;
