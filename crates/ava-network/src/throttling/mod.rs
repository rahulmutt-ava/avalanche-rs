// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Networking throttlers (`specs/05` §5), reproducing `network/throttling/*`.
//!
//! Currently provides the outbound message byte throttler ([`outbound_msg`]).
//! The dial, inbound-connection, inbound-byte, bandwidth, buffer, and resource
//! throttlers land in later waves.

pub mod outbound_msg;
