// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Shared test helpers: open a real `FirewoodStateProvider` over in-memory side
//! stores and commit distinct EVM state roots through its propose/stash/commit
//! lifecycle (mirrors `ava-evm`'s own `state.rs` test setup).

use std::sync::Arc;

use ava_database::{DynDatabase, MemDb};
use ava_evm::{FirewoodStateProvider, hashed_post_state_to_batchops};
use ava_evm_reth::{Account, Address, B256, B256Map, HashedPostState, U256, keccak256};

/// Opens a fresh provider backed by a temp dir + in-memory side KVs. Holds the
/// `TempDir` alive for the life of the returned tuple.
pub fn open_provider() -> (tempfile::TempDir, Arc<FirewoodStateProvider>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider = FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open");
    (dir, provider)
}

/// Builds a one-account `HashedPostState` whose nonce/balance are seeded from
/// `seed`, proposes + stashes it against the current tip, and returns the
/// post-state root **without** committing. Distinct seeds yield distinct roots.
pub fn propose_root(provider: &Arc<FirewoodStateProvider>, seed: u64) -> B256 {
    let mut accounts = B256Map::default();
    accounts.insert(
        keccak256(Address::repeat_byte(
            u8::try_from(seed & 0xff).expect("byte"),
        )),
        Some(Account {
            nonce: seed.wrapping_add(1),
            balance: U256::from(seed.wrapping_add(1)),
            bytecode_hash: None,
        }),
    );
    let hashed = HashedPostState {
        accounts,
        storages: B256Map::default(),
    };
    let ops = hashed_post_state_to_batchops(&hashed);
    provider.propose_and_stash(ops).expect("propose")
}
