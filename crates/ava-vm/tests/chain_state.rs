// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `chain_state_caches` — the `chain::State` block-cache decorator (specs 07
//! §3.3): block cache tiers + idempotent get/parse + `last_accepted` tracking
//! through the `BlockWrapper` lifecycle.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use ava_snow::{Block, Result as SnowResult};
use ava_types::id::Id;
use ava_vm::components::chain::{ChainState, ChainStateConfig};

/// A trivial raw block. `bytes` is `id (32) ++ be64(height)`.
#[derive(Debug)]
struct RawBlock {
    id: Id,
    parent: Id,
    height: u64,
    bytes: Vec<u8>,
}

impl RawBlock {
    fn new(id_byte: u8, parent: Id, height: u64) -> Arc<Self> {
        let id = Id::from([id_byte; 32]);
        let mut bytes = id.to_bytes().to_vec();
        bytes.extend_from_slice(&height.to_be_bytes());
        Arc::new(Self {
            id,
            parent,
            height,
            bytes,
        })
    }
}

#[async_trait]
impl Block for RawBlock {
    fn id(&self) -> Id {
        self.id
    }
    fn parent(&self) -> Id {
        self.parent
    }
    fn height(&self) -> u64 {
        self.height
    }
    fn timestamp(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    async fn verify(&self, _t: &CancellationToken) -> SnowResult<()> {
        Ok(())
    }
    async fn accept(&self, _t: &CancellationToken) -> SnowResult<()> {
        Ok(())
    }
    async fn reject(&self, _t: &CancellationToken) -> SnowResult<()> {
        Ok(())
    }
}

/// A shared block store the closures read from.
type Store = Arc<Mutex<HashMap<Id, Arc<RawBlock>>>>;

fn config(store: Store, last_accepted: Arc<RawBlock>) -> ChainStateConfig<RawBlock> {
    let get_store = Arc::clone(&store);
    let unmarshal_store = Arc::clone(&store);
    ChainStateConfig {
        decided_cache_size: 10,
        missing_cache_size: 10,
        unverified_cache_size: 10,
        bytes_to_id_cache_size: 10,
        last_accepted,
        get_block: Box::new(move |_t, id| {
            let store = Arc::clone(&get_store);
            Box::pin(async move {
                store
                    .lock()
                    .unwrap()
                    .get(&id)
                    .map(Arc::clone)
                    .ok_or(ava_vm::error::Error::NotFound)
            }) as BoxFuture<'static, _>
        }),
        unmarshal: Box::new(move |_t, bytes| {
            let store = Arc::clone(&unmarshal_store);
            Box::pin(async move {
                // bytes = id(32) ++ be64(height); look it up by id.
                let mut id_bytes = [0u8; 32];
                id_bytes.copy_from_slice(&bytes[..32]);
                let id = Id::from(id_bytes);
                store
                    .lock()
                    .unwrap()
                    .get(&id)
                    .map(Arc::clone)
                    .ok_or(ava_vm::error::Error::NotFound)
            }) as BoxFuture<'static, _>
        }),
        build_block: Box::new(move |_t| {
            Box::pin(async move { Err(ava_vm::error::Error::NotFound) }) as BoxFuture<'static, _>
        }),
    }
}

#[tokio::test]
async fn chain_state_caches() {
    let token = CancellationToken::new();
    let store: Store = Arc::new(Mutex::new(HashMap::new()));

    // Genesis (height 0) + a child b1 (height 1).
    let genesis = RawBlock::new(0x00, Id::EMPTY, 0);
    let b1 = RawBlock::new(0x01, genesis.id(), 1);
    {
        let mut s = store.lock().unwrap();
        s.insert(genesis.id(), Arc::clone(&genesis));
        s.insert(b1.id(), Arc::clone(&b1));
    }

    let state = ChainState::new(config(Arc::clone(&store), Arc::clone(&genesis)));

    // last_accepted is genesis.
    assert_eq!(state.last_accepted(), genesis.id());

    // get_block is idempotent: two fetches yield the same Arc.
    let g1 = state
        .get_block(&token, genesis.id())
        .await
        .expect("get genesis");
    let g2 = state
        .get_block(&token, genesis.id())
        .await
        .expect("get genesis 2");
    assert!(Arc::ptr_eq(&g1, &g2), "get_block is idempotent (same Arc)");

    // Fetching b1 (height 1 > last_accepted height 0) tiers it as unverified.
    let w1 = state.get_block(&token, b1.id()).await.expect("get b1");
    assert!(!state.is_processing(b1.id()), "not yet verified");

    // parse_block round-trips and dedups against the cached block.
    let parsed = state
        .parse_block(&token, b1.bytes())
        .await
        .expect("parse b1");
    assert!(
        Arc::ptr_eq(&w1, &parsed),
        "parse dedups to the cached wrapper"
    );

    // Verify moves b1 into verified_blocks (in consensus).
    w1.verify(&token).await.expect("verify b1");
    assert!(state.is_processing(b1.id()), "verified ⇒ processing");

    // Accept advances last_accepted and removes from verified_blocks.
    w1.accept(&token).await.expect("accept b1");
    assert!(
        !state.is_processing(b1.id()),
        "accepted ⇒ no longer processing"
    );
    assert_eq!(
        state.last_accepted(),
        b1.id(),
        "accept advances last_accepted"
    );

    // get_block of the now-accepted b1 returns from the decided cache.
    let again = state
        .get_block(&token, b1.id())
        .await
        .expect("get accepted b1");
    assert_eq!(again.id(), b1.id());

    // Unknown id ⇒ NotFound, cached as missing (a second miss is also NotFound).
    let unknown = Id::from([0xEE; 32]);
    assert!(matches!(
        state.get_block(&token, unknown).await,
        Err(ava_vm::error::Error::NotFound)
    ));
    assert!(matches!(
        state.get_block(&token, unknown).await,
        Err(ava_vm::error::Error::NotFound)
    ));
}
