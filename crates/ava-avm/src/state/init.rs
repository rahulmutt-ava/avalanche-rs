// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `state.InitializeChainState` — genesis Snowman block seeding (specs 09 §1/§5/§5.3, 07).
//!
//! Port of `vms/avm/state/state.go`'s `InitializeChainState(stopVertexID,
//! genesisTimestamp)`. After linearization the X-Chain's first Snowman block is
//! a height-0 [`StandardBlock`](crate::block::StandardBlock) whose parent is the
//! DAG **stop vertex** (specs 09 §1). On a fresh chain (no stored
//! `lastAccepted`) this builds + persists that genesis block and sets the
//! singleton flags; on an already-initialized chain it loads the persisted
//! `lastAccepted`/`timestamp` back into the in-memory fields and returns.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_codec::manager::Manager;
use ava_types::id::Id;

use crate::block::StandardBlock;
use crate::error::Result;
use crate::state::chain::Chain;
use crate::state::state::State;

use ava_database::Database;

impl<D: Database> State<D> {
    /// `InitializeChainState` — seed the genesis Snowman block on a fresh chain,
    /// or load the persisted singletons on an already-initialized one
    /// (`state.go`, specs 09 §1/§5.3).
    ///
    /// On a fresh chain (no persisted last-accepted block) this builds a genesis
    /// [`StandardBlock`] with `parent = stop_vertex_id`, `height = 0`,
    /// `time = genesis_ts` (truncated to Unix seconds), and no transactions,
    /// then persists it: `add_block` (block store + 8-byte big-endian
    /// `height → blockID` index), `set_last_accepted`, `set_timestamp`
    /// (Unix-second value), and `set_initialized`, committing the batch. The
    /// genesis block is built with the **standard** codec `c` (Go
    /// `s.parser.Codec()`).
    ///
    /// Idempotent: a subsequent call observes the persisted state via
    /// [`load`](State::load) and does **not** re-seed.
    ///
    /// # Errors
    /// Returns [`Error::Codec`](crate::error::Error::Codec) if the genesis block
    /// fails to marshal, or [`Error::Database`](crate::error::Error::Database) if
    /// loading the persisted singletons or committing the seed fails.
    pub fn initialize_chain_state(
        &mut self,
        stop_vertex_id: Id,
        genesis_ts: SystemTime,
        c: &Manager,
    ) -> Result<()> {
        // Already initialized: load the persisted last-accepted + timestamp back
        // into the in-memory fields and return (Go reads `lastAcceptedKey`; a
        // missing key means a fresh chain).
        if self.is_initialized()? {
            self.load()?;
            return Ok(());
        }

        // Fresh chain: build + persist the genesis block. Truncate the genesis
        // timestamp to whole Unix seconds — the block `Time` field and the
        // singleton timestamp are both Unix-second `u64`s.
        let genesis_secs = genesis_ts
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        let genesis = StandardBlock::new_block(c, stop_vertex_id, 0, genesis_secs, Vec::new())?;

        self.set_last_accepted(genesis.id());
        // Re-derive the SystemTime from the truncated seconds so the persisted
        // timestamp matches the block's `Time` exactly.
        self.set_timestamp(
            UNIX_EPOCH
                .checked_add(Duration::from_secs(genesis_secs))
                .unwrap_or(UNIX_EPOCH),
        );
        self.add_block(genesis.id(), genesis.height(), genesis.bytes().to_vec());
        self.set_initialized()?;

        self.commit()
    }
}
