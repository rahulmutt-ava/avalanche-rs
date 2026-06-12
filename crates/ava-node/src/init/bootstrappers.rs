// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init step 5 (specs/12 §2.2): the bootstrap-beacon validator set (mirror Go
//! `initBootstrappers`).

use std::sync::Arc;

use ava_genesis::Bootstrapper;
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::id::Id;
use ava_validators::{DefaultManager, ValidatorManager};

use crate::error::{Error, Result};

/// Build the beacon set: every configured bootstrapper is added to the primary
/// network with weight 1 (the beacon connection manager treats all beacons as
/// equal; the TxID / BLS key are never used — Go invariant comment).
///
/// # Errors
/// [`Error::Bootstrappers`] when a beacon cannot be added (duplicate id).
pub fn new_bootstrappers(bootstrappers: &[Bootstrapper]) -> Result<Arc<dyn ValidatorManager>> {
    let beacons: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
    for bootstrapper in bootstrappers {
        beacons
            .add_staker(PRIMARY_NETWORK_ID, bootstrapper.id, None, Id::EMPTY, 1)
            .map_err(|e| Error::Bootstrappers(e.to_string()))?;
    }
    Ok(beacons)
}
