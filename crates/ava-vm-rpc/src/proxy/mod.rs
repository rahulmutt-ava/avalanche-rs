// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The proxied callback services (specs 07 §5.4; plan M3.25).
//!
//! Whichever side is the *plugin* always **dials** the callback services;
//! whichever side is the *node* always **serves** them. So each module ships a
//! guest-side client implementing a Rust trait and a host-side tonic server
//! wrapping the host's implementation of that trait.

pub mod aliasreader;
pub mod appsender;
pub mod rpcdb;
pub mod sharedmemory;
pub mod validatorstate;
pub mod warp;
