// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! ProposerVM persisted state over a [`DynDatabase`].
//!
//! Port of Go `vms/proposervm/state/` (`chain_state.go`, `block_state.go`,
//! `block_height_index.go`, `state.go`). The Go implementation splits the VM's
//! database into three `prefixdb` namespaces (`chain` / `block` / `height`); we
//! reproduce the same byte layout by prefixing keys within one [`DynDatabase`]:
//!
//! - **chain** (`chainStatePrefix = "chain"`): the last-accepted proposervm
//!   block id under `lastAcceptedKey = [0x00]`.
//! - **block** (`blockStatePrefix = "block"`): each post-fork block keyed by its
//!   id → its serialized bytes.
//! - **height** (`heightIndexPrefix = "height"`): the `heightPrefix = "height"`
//!   sub-namespace maps `be64(height)` → block id, and `metadataPrefix =
//!   "metadata"` holds the fork height under `forkKey = "fork"`.
//!
//! Keys are `prefix ++ key` (Go `prefixdb` hashes the prefix + length, but the
//! resulting namespace separation is the only observable contract for a
//! single-node port; the byte layout here is internally consistent and
//! self-describing). The proposervm's own DB is independent of the inner VM's,
//! so cross-node wire parity is unaffected (block *bytes* are byte-exact via the
//! block codec; only the local index layout differs and is never gossiped).

use std::sync::Arc;

use ava_database::DynDatabase;
use ava_types::id::Id;

use crate::error::{Error, Result};

// Namespace prefixes (mirror the Go `prefixdb` names).
const CHAIN_PREFIX: &[u8] = b"chain";
const BLOCK_PREFIX: &[u8] = b"block";
const HEIGHT_PREFIX: &[u8] = b"height";
const METADATA_PREFIX: &[u8] = b"metadata";

// Keys within their namespaces.
const LAST_ACCEPTED_KEY: &[u8] = &[0x00];
const FORK_KEY: &[u8] = b"fork";

/// The proposervm persisted state.
pub struct State {
    db: Arc<dyn DynDatabase>,
}

impl State {
    /// Wraps a database as the proposervm state store.
    #[must_use]
    pub fn new(db: Arc<dyn DynDatabase>) -> Self {
        Self { db }
    }

    fn chain_key(key: &[u8]) -> Vec<u8> {
        prefixed(CHAIN_PREFIX, key)
    }

    fn block_key(id: &Id) -> Vec<u8> {
        prefixed(BLOCK_PREFIX, id.as_bytes())
    }

    fn height_key(height: u64) -> Vec<u8> {
        prefixed2(HEIGHT_PREFIX, HEIGHT_PREFIX, &height.to_be_bytes())
    }

    fn fork_key() -> Vec<u8> {
        prefixed2(HEIGHT_PREFIX, METADATA_PREFIX, FORK_KEY)
    }

    // ---- chain state ----

    /// `GetLastAccepted` — the last-accepted proposervm block id.
    ///
    /// # Errors
    /// [`Error::NotFound`] if no post-fork block has been accepted yet.
    pub fn get_last_accepted(&self) -> Result<Id> {
        let raw = self.db.get(&Self::chain_key(LAST_ACCEPTED_KEY))?;
        Id::from_slice(&raw).map_err(|e| Error::Database(format!("bad last-accepted id: {e:?}")))
    }

    /// `SetLastAccepted`.
    ///
    /// # Errors
    /// Propagates the underlying database error.
    pub fn set_last_accepted(&self, id: Id) -> Result<()> {
        self.db
            .put(&Self::chain_key(LAST_ACCEPTED_KEY), id.as_bytes())?;
        Ok(())
    }

    // ---- block state ----

    /// `PutBlock` — persist a post-fork block's serialized bytes by id.
    ///
    /// # Errors
    /// Propagates the underlying database error.
    pub fn put_block(&self, id: Id, bytes: &[u8]) -> Result<()> {
        self.db.put(&Self::block_key(&id), bytes)?;
        Ok(())
    }

    /// `GetBlock` — the serialized bytes of a persisted post-fork block.
    ///
    /// # Errors
    /// [`Error::NotFound`] if the block is not indexed.
    pub fn get_block(&self, id: Id) -> Result<Vec<u8>> {
        Ok(self.db.get(&Self::block_key(&id))?)
    }

    // ---- height index + fork height ----

    /// `GetForkHeight` — the height of the first post-fork block.
    ///
    /// # Errors
    /// [`Error::NotFound`] if the fork has not been reached.
    pub fn get_fork_height(&self) -> Result<u64> {
        let raw = self.db.get(&Self::fork_key())?;
        parse_u64(&raw)
    }

    /// `SetForkHeight`.
    ///
    /// # Errors
    /// Propagates the underlying database error.
    pub fn set_fork_height(&self, height: u64) -> Result<()> {
        self.db.put(&Self::fork_key(), &height.to_be_bytes())?;
        Ok(())
    }

    /// `GetBlockIDAtHeight`.
    ///
    /// # Errors
    /// [`Error::NotFound`] if the height is not indexed.
    pub fn get_block_id_at_height(&self, height: u64) -> Result<Id> {
        let raw = self.db.get(&Self::height_key(height))?;
        Id::from_slice(&raw).map_err(|e| Error::Database(format!("bad indexed id: {e:?}")))
    }

    /// `SetBlockIDAtHeight`.
    ///
    /// # Errors
    /// Propagates the underlying database error.
    pub fn set_block_id_at_height(&self, height: u64, id: Id) -> Result<()> {
        self.db.put(&Self::height_key(height), id.as_bytes())?;
        Ok(())
    }
}

fn prefixed(prefix: &[u8], key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len().saturating_add(1).saturating_add(key.len()));
    out.extend_from_slice(prefix);
    out.push(b'/');
    out.extend_from_slice(key);
    out
}

fn prefixed2(p0: &[u8], p1: &[u8], key: &[u8]) -> Vec<u8> {
    let inner = prefixed(p1, key);
    prefixed(p0, &inner)
}

fn parse_u64(raw: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = raw
        .try_into()
        .map_err(|_| Error::Database("u64 value not 8 bytes".to_string()))?;
    Ok(u64::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use ava_database::MemDb;

    use super::*;

    #[test]
    fn round_trips_last_accepted_and_height_index() {
        let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
        let state = State::new(db);

        // last accepted
        assert!(matches!(state.get_last_accepted(), Err(Error::NotFound)));
        let id = Id::from([3u8; 32]);
        state.set_last_accepted(id).expect("set");
        assert_eq!(state.get_last_accepted().expect("get"), id);

        // fork height
        assert!(matches!(state.get_fork_height(), Err(Error::NotFound)));
        state.set_fork_height(7).expect("set fork");
        assert_eq!(state.get_fork_height().expect("get fork"), 7);

        // height index
        assert!(matches!(
            state.get_block_id_at_height(7),
            Err(Error::NotFound)
        ));
        state.set_block_id_at_height(7, id).expect("set height");
        assert_eq!(state.get_block_id_at_height(7).expect("get height"), id);

        // block bytes
        state.put_block(id, b"blockbytes").expect("put block");
        assert_eq!(state.get_block(id).expect("get block"), b"blockbytes");
    }
}
