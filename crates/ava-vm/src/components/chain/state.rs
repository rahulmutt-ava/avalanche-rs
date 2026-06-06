// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `vms/components/chain.State` — the caching block decorator (specs 07 §3.3).
//!
//! A VM wraps its raw block storage in [`ChainState`] to get idempotent
//! `get_block`/`parse_block`/`build_block` (a given block id always yields the
//! same `Arc`) and to dedup in-flight verification. The [`BlockWrapper`]
//! intercepts `verify`/`accept`/`reject` to move a block between cache tiers and
//! keep `last_accepted` correct (Go `chain/block.go`).
//!
//! ## Cache tiers (Go parity)
//!
//! * `verified_blocks` — blocks currently in consensus (verified, not decided).
//! * `decided` — an LRU of decided (accepted/rejected) blocks.
//! * `unverified` — an LRU of processing-but-unverified blocks.
//! * `missing` — an LRU of block ids known to be absent (cacheable `NotFound`).
//! * `bytes_to_id` — an LRU mapping a block's bytes to its id (so `parse_block`
//!   can short-circuit).
//!
//! **Deviation from Go:** Go's caches are *byte-sized* (`lru.NewSizedCache`);
//! here they are *count-bounded* LRUs ([`CountLru`]). The tiering, idempotency,
//! and `last_accepted` semantics — the parts that are observable / golden-tested
//! — are identical; only the eviction *metric* differs. Swap in a byte-sized LRU
//! when `ava-utils` grows one (recorded in `tests/PORTING.md`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use ava_snow::{Block, Result as SnowResult};
use ava_types::id::Id;

use crate::error::{Error, Result};

/// A minimal count-bounded LRU (insertion/most-recent-use ordered).
///
/// Keeps at most `cap` entries; on overflow the least-recently-used entry is
/// evicted. `cap == 0` disables caching (every put is a no-op). Determinism is
/// not at stake here (these are local performance caches, never serialized), so
/// a plain `HashMap` + recency `Vec` is fine.
#[derive(Debug)]
struct CountLru<K, V> {
    cap: usize,
    map: HashMap<K, V>,
    // Most-recently-used keys at the back.
    order: Vec<K>,
}

impl<K: Clone + Eq + std::hash::Hash, V> CountLru<K, V> {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            map: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn touch(&mut self, key: &K) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(pos);
            self.order.push(k);
        }
    }

    fn get(&mut self, key: &K) -> Option<&V> {
        if self.map.contains_key(key) {
            self.touch(key);
            self.map.get(key)
        } else {
            None
        }
    }

    fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    fn put(&mut self, key: K, value: V) {
        if self.cap == 0 {
            return;
        }
        if self.map.insert(key.clone(), value).is_some() {
            self.touch(&key);
            return;
        }
        self.order.push(key);
        while self.order.len() > self.cap {
            if self.order.is_empty() {
                break;
            }
            let evict = self.order.remove(0);
            self.map.remove(&evict);
        }
    }

    fn evict(&mut self, key: &K) {
        if self.map.remove(key).is_none() {
            return;
        }
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            let _ = self.order.remove(pos);
        }
    }
}

/// The shared, mutable cache state behind a [`ChainState`] and its
/// [`BlockWrapper`]s.
struct CacheInner<B: Block> {
    /// Blocks currently in consensus (verified, awaiting a decision).
    verified_blocks: HashMap<Id, Arc<BlockWrapper<B>>>,
    /// LRU of decided blocks.
    decided: CountLru<Id, Arc<BlockWrapper<B>>>,
    /// LRU of processing-but-unverified blocks.
    unverified: CountLru<Id, Arc<BlockWrapper<B>>>,
    /// LRU of ids known to be absent.
    missing: CountLru<Id, ()>,
    /// LRU mapping block bytes → id.
    bytes_to_id: CountLru<Vec<u8>, Id>,
    /// The last accepted (wrapped) block.
    last_accepted: Arc<BlockWrapper<B>>,
    /// The height of the last accepted block (cached for `add_outside_consensus`
    /// tier selection).
    last_accepted_height: u64,
}

/// Configuration for building a [`ChainState`] (Go `chain.Config`).
///
/// The three closures bridge to the VM's raw storage; each returns a
/// [`BoxFuture`] so it can be async. `last_accepted` seeds the decided cache.
pub struct ChainStateConfig<B: Block> {
    /// Capacity of the decided-block LRU.
    pub decided_cache_size: usize,
    /// Capacity of the missing-block LRU.
    pub missing_cache_size: usize,
    /// Capacity of the unverified-block LRU.
    pub unverified_cache_size: usize,
    /// Capacity of the bytes→id LRU.
    pub bytes_to_id_cache_size: usize,
    /// The last accepted block at construction time.
    pub last_accepted: Arc<B>,
    /// `GetBlock` — fetch a raw block by id (`Err(NotFound)` ⇒ cacheable miss).
    #[allow(clippy::type_complexity)]
    pub get_block:
        Box<dyn Fn(CancellationToken, Id) -> BoxFuture<'static, Result<Arc<B>>> + Send + Sync>,
    /// `UnmarshalBlock` — parse a raw block from bytes.
    #[allow(clippy::type_complexity)]
    pub unmarshal:
        Box<dyn Fn(CancellationToken, Vec<u8>) -> BoxFuture<'static, Result<Arc<B>>> + Send + Sync>,
    /// `BuildBlock` — build a new raw block on the preference.
    #[allow(clippy::type_complexity)]
    pub build_block:
        Box<dyn Fn(CancellationToken) -> BoxFuture<'static, Result<Arc<B>>> + Send + Sync>,
}

/// `chain.State` — the caching block decorator.
pub struct ChainState<B: Block> {
    inner: Arc<Mutex<CacheInner<B>>>,
    config: ChainStateConfig<B>,
}

impl<B: Block + 'static> ChainState<B> {
    /// Builds a [`ChainState`] from its config, seeding the decided cache with
    /// the last accepted block.
    #[must_use]
    pub fn new(config: ChainStateConfig<B>) -> Self {
        let la_id = config.last_accepted.id();
        let la_height = config.last_accepted.height();

        // The wrapper for the last accepted block needs a back-reference to the
        // cache, so build the inner with a placeholder, then patch it.
        let inner = Arc::new(Mutex::new(CacheInner {
            verified_blocks: HashMap::new(),
            decided: CountLru::new(config.decided_cache_size),
            unverified: CountLru::new(config.unverified_cache_size),
            missing: CountLru::new(config.missing_cache_size),
            bytes_to_id: CountLru::new(config.bytes_to_id_cache_size),
            last_accepted: Arc::new(BlockWrapper {
                block: Arc::clone(&config.last_accepted),
                inner: Mutex::new(None),
            }),
            last_accepted_height: la_height,
        }));

        // Patch the wrapper's back-reference + register it in the decided cache.
        {
            let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
            let la_wrapper = Arc::clone(&guard.last_accepted);
            *la_wrapper.inner.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&inner));
            guard.decided.put(la_id, la_wrapper);
        }

        Self { inner, config }
    }

    /// `LastAccepted()` — the id of the last accepted block.
    #[must_use]
    pub fn last_accepted(&self) -> Id {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.last_accepted.id()
    }

    /// `IsProcessing(blkID)` — whether `blk_id` is currently in consensus.
    #[must_use]
    pub fn is_processing(&self, blk_id: Id) -> bool {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.verified_blocks.contains_key(&blk_id)
    }

    /// Looks up `blk_id` across the cache tiers (verified → decided →
    /// unverified). Returns the wrapped block if cached.
    fn get_cached(&self, blk_id: Id) -> Option<Arc<BlockWrapper<B>>> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = guard.verified_blocks.get(&blk_id) {
            return Some(Arc::clone(b));
        }
        if let Some(b) = guard.decided.get(&blk_id) {
            return Some(Arc::clone(b));
        }
        if let Some(b) = guard.unverified.get(&blk_id) {
            return Some(Arc::clone(b));
        }
        None
    }

    /// `GetBlock(ctx, blkID)` — idempotent fetch. `Err(NotFound)` is cached in
    /// the missing LRU.
    ///
    /// # Errors
    /// Returns [`Error::NotFound`] for an absent block, or any error from the
    /// underlying `get_block` closure.
    pub async fn get_block(
        &self,
        token: &CancellationToken,
        blk_id: Id,
    ) -> Result<Arc<BlockWrapper<B>>> {
        if let Some(b) = self.get_cached(blk_id) {
            return Ok(b);
        }
        {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if guard.missing.contains(&blk_id) {
                return Err(Error::NotFound);
            }
        }

        match (self.config.get_block)(token.clone(), blk_id).await {
            Ok(blk) => Ok(self.add_outside_consensus(blk)),
            Err(Error::NotFound) => {
                let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard.missing.put(blk_id, ());
                Err(Error::NotFound)
            }
            Err(e) => Err(e),
        }
    }

    /// `ParseBlock(ctx, b)` — parse + cache, returning a unique wrapper per id.
    ///
    /// # Errors
    /// Returns any error from the underlying `unmarshal` closure.
    pub async fn parse_block(
        &self,
        token: &CancellationToken,
        bytes: &[u8],
    ) -> Result<Arc<BlockWrapper<B>>> {
        // Short-circuit via the bytes→id cache.
        let cached_id = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.bytes_to_id.get(&bytes.to_vec()).copied()
        };
        if let Some(b) = cached_id.and_then(|id| self.get_cached(id)) {
            return Ok(b);
        }

        let blk = (self.config.unmarshal)(token.clone(), bytes.to_vec()).await?;
        let blk_id = blk.id();
        {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.bytes_to_id.put(bytes.to_vec(), blk_id);
        }

        // If we did not consult the caches above (id was not byte-cached), do so
        // now so a concurrently-cached equivalent block is returned.
        #[allow(clippy::collapsible_if)] // let-chains are not stable.
        if cached_id.is_none() {
            if let Some(b) = self.get_cached(blk_id) {
                return Ok(b);
            }
        }

        {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.missing.evict(&blk_id);
        }
        Ok(self.add_outside_consensus(blk))
    }

    /// `BuildBlock(ctx)` — build + dedup against the caches.
    ///
    /// # Errors
    /// Returns any error from the underlying `build_block` closure.
    pub async fn build_block(&self, token: &CancellationToken) -> Result<Arc<BlockWrapper<B>>> {
        let blk = (self.config.build_block)(token.clone()).await?;
        let blk_id = blk.id();
        if let Some(existing) = self.get_cached(blk_id) {
            return Ok(existing);
        }
        {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.missing.evict(&blk_id);
        }
        Ok(self.add_outside_consensus(blk))
    }

    /// Wraps `blk` and files it under the decided cache (if `height <=
    /// last_accepted`) or the unverified cache (otherwise).
    fn add_outside_consensus(&self, blk: Arc<B>) -> Arc<BlockWrapper<B>> {
        let wrapper = Arc::new(BlockWrapper {
            block: Arc::clone(&blk),
            inner: Mutex::new(Some(Arc::clone(&self.inner))),
        });
        let blk_id = blk.id();
        let height = blk.height();

        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if height <= guard.last_accepted_height {
            guard.decided.put(blk_id, Arc::clone(&wrapper));
        } else {
            guard.unverified.put(blk_id, Arc::clone(&wrapper));
        }
        wrapper
    }
}

/// `chain.BlockWrapper` — wraps a raw block and intercepts its lifecycle to keep
/// the [`ChainState`] caches coherent.
pub struct BlockWrapper<B: Block> {
    block: Arc<B>,
    // Back-reference to the owning cache. `None` only transiently during
    // construction of the genesis wrapper.
    inner: Mutex<Option<Arc<Mutex<CacheInner<B>>>>>,
}

impl<B: Block> BlockWrapper<B> {
    /// The wrapped raw block.
    #[must_use]
    pub fn inner_block(&self) -> &Arc<B> {
        &self.block
    }

    fn cache(&self) -> Option<Arc<Mutex<CacheInner<B>>>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl<B: Block + 'static> Block for BlockWrapper<B> {
    fn id(&self) -> Id {
        self.block.id()
    }

    fn parent(&self) -> Id {
        self.block.parent()
    }

    fn height(&self) -> u64 {
        self.block.height()
    }

    fn timestamp(&self) -> SystemTime {
        self.block.timestamp()
    }

    fn bytes(&self) -> &[u8] {
        self.block.bytes()
    }

    async fn verify(&self, token: &CancellationToken) -> SnowResult<()> {
        // Cannot cache a verification failure (the error may be transient).
        self.block.verify(token).await?;
        if let Some(cache) = self.cache() {
            let blk_id = self.block.id();
            let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            guard.unverified.evict(&blk_id);
            let wrapper = Arc::new(BlockWrapper {
                block: Arc::clone(&self.block),
                inner: Mutex::new(Some(Arc::clone(&cache))),
            });
            guard.verified_blocks.insert(blk_id, wrapper);
        }
        Ok(())
    }

    async fn accept(&self, token: &CancellationToken) -> SnowResult<()> {
        if let Some(cache) = self.cache() {
            let blk_id = self.block.id();
            let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            guard.verified_blocks.remove(&blk_id);
            let wrapper = Arc::new(BlockWrapper {
                block: Arc::clone(&self.block),
                inner: Mutex::new(Some(Arc::clone(&cache))),
            });
            guard.decided.put(blk_id, Arc::clone(&wrapper));
            guard.last_accepted_height = self.block.height();
            guard.last_accepted = wrapper;
        }
        self.block.accept(token).await
    }

    async fn reject(&self, token: &CancellationToken) -> SnowResult<()> {
        if let Some(cache) = self.cache() {
            let blk_id = self.block.id();
            let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            guard.verified_blocks.remove(&blk_id);
            let wrapper = Arc::new(BlockWrapper {
                block: Arc::clone(&self.block),
                inner: Mutex::new(Some(Arc::clone(&cache))),
            });
            guard.decided.put(blk_id, wrapper);
        }
        self.block.reject(token).await
    }
}
