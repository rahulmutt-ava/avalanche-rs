// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Simplex consensus tunables (port of
//! `snow/consensus/simplex/parameters.go`, specs 06 §8).
//!
//! Unlike Snowman there is **no** `k`/`alpha`/`beta` metastable schedule —
//! safety is a BLS quorum (⅔ of the fixed validator set), so the only knobs are
//! timing (`max_network_delay`, `max_rebroadcast_wait`) plus the initial
//! validator membership.

use std::time::Duration;

use ava_crypto::bls::PublicKey;
use ava_types::node_id::NodeId;

use crate::error::{Error, Result};

/// A single member of the Simplex validator set (`simplex.ValidatorInfo`).
#[derive(Clone)]
pub struct ValidatorInfo {
    /// The validator's node ID.
    pub node_id: NodeId,
    /// The validator's BLS public key, in **compressed** (48-byte) form, as it
    /// appears on the wire and is fed to [`PublicKey::from_compressed`].
    pub public_key: Vec<u8>,
}

impl ValidatorInfo {
    /// Parses [`Self::public_key`] into a [`PublicKey`], validating the
    /// subgroup membership (Go `bls.PublicKeyFromCompressedBytes`).
    pub fn parse_public_key(&self) -> Result<PublicKey> {
        PublicKey::from_compressed(&self.public_key).map_err(|e| Error::InvalidPublicKey {
            node_id: self.node_id,
            source: e,
        })
    }
}

/// Simplex consensus parameters (`simplex.Parameters`).
#[derive(Clone)]
pub struct Parameters {
    /// Upper bound on the network message delay used to size view-change
    /// timeouts.
    pub max_network_delay: Duration,
    /// Upper bound on how long the engine waits before rebroadcasting.
    pub max_rebroadcast_wait: Duration,
    /// The initial validator membership set.
    pub initial_validators: Vec<ValidatorInfo>,
}

/// `simplex.DefaultParameters` — 5s delays and an **empty** validator set (the
/// caller must supply `initial_validators` before [`Parameters::verify`]).
pub const DEFAULT_MAX_NETWORK_DELAY: Duration = Duration::from_secs(5);
/// `simplex.DefaultParameters.MaxRebroadcastWait`.
pub const DEFAULT_MAX_REBROADCAST_WAIT: Duration = Duration::from_secs(5);

impl Default for Parameters {
    /// Mirrors Go's `DefaultParameters`: 5s delays, no initial validators.
    fn default() -> Self {
        Self {
            max_network_delay: DEFAULT_MAX_NETWORK_DELAY,
            max_rebroadcast_wait: DEFAULT_MAX_REBROADCAST_WAIT,
            initial_validators: Vec::new(),
        }
    }
}

impl Parameters {
    /// `Parameters.Verify` — validates the parameters in Go's exact branch
    /// order. Returns [`Error::InvalidParameters`] on the first failing check.
    pub fn verify(&self) -> Result<()> {
        if self.max_network_delay.is_zero() {
            return Err(Error::InvalidParameters("maxNetworkDelay must be positive"));
        }
        if self.max_rebroadcast_wait.is_zero() {
            return Err(Error::InvalidParameters(
                "maxRebroadcastWait must be positive",
            ));
        }
        if self.initial_validators.is_empty() {
            return Err(Error::InvalidParameters(
                "initialValidators must be non-empty",
            ));
        }
        Ok(())
    }
}
