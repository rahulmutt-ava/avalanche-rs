// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Networking throttlers (`specs/05` §5), reproducing `network/throttling/*`.
//!
//! Each throttler reproduces a Go throttler's accounting exactly while replacing
//! the `ReleaseFunc` footgun with RAII permits where applicable:
//!
//! - [`outbound_msg`] — outbound message byte throttler (non-blocking, drops on
//!   refusal) (M2.12).
//! - [`inbound_msg_byte`] — inbound message byte throttler (3 fair pools,
//!   blocking acquire) (M2.13).
//! - [`inbound_conn_upgrade`] — inbound connection-upgrade throttler (per-IP
//!   cooldown + global rate cap) (M2.13).
//!
//! The dial, bandwidth, buffer, and resource throttlers land in later waves
//! (M2.18+).

pub mod inbound_conn_upgrade;
pub mod inbound_msg_byte;
pub mod outbound_msg;
