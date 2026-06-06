// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Inbound/outbound throttlers (port of `network/throttling/*`).
//!
//! See `specs/05-networking-p2p.md` §5. Each throttler reproduces a Go
//! throttler's accounting exactly while replacing the `ReleaseFunc` footgun
//! with RAII permits where applicable.

pub mod inbound_conn_upgrade;
pub mod inbound_msg_byte;
