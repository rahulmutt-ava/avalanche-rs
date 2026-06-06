// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Composable [`ValidatorState`] adapters mirroring Go's `cachedState` and
//! `lockedState`.
//!
//! - [`CachedState`] memoizes `get_validator_set(height, subnet)` results in a
//!   bounded LRU (Go `validators.cachedState`). The cache key is `(height, subnet)`;
//!   entries are evicted least-recently-used once the capacity is reached.
//! - [`LockedState`] serializes all access to an inner state behind a
//!   `tokio::sync::Mutex` (Go `validators.lockedState`).
//!
//! Both adapters wrap any `Arc<dyn ValidatorState>` and are themselves
//! `ValidatorState`, so they compose (`LockedState<CachedState<...>>` etc.).
//!
//! > **Note (deviation):** `specs/06` §6.1 says "LRU via ava-utils", but `ava-utils`
//! > exposes no cache module yet. To avoid coupling to an unbuilt crate / adding a
//! > new dependency, `CachedState` carries a minimal self-contained insertion-order
//! > LRU. If `ava-utils` later grows an `lru` module this should be swapped to it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::Mutex as AsyncMutex;

use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::error::Result;
use crate::state::ValidatorState;
use crate::validator::GetValidatorOutput;

/// A cached, shareable validator-set value.
type CachedSet = Arc<BTreeMap<NodeId, GetValidatorOutput>>;
/// LRU cache key: `(p_chain_height, subnet_id)`.
type CacheKey = (u64, Id);

/// A minimal bounded LRU keyed by `(height, subnet)`. Single-purpose, internal.
struct Lru {
    capacity: usize,
    /// (key, value) in least-recently-used → most-recently-used order.
    entries: Vec<(CacheKey, CachedSet)>,
}

impl Lru {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Vec::new(),
        }
    }

    fn get(&mut self, key: CacheKey) -> Option<CachedSet> {
        let pos = self.entries.iter().position(|(k, _)| *k == key)?;
        let entry = self.entries.remove(pos);
        let value = Arc::clone(&entry.1);
        self.entries.push(entry);
        Some(value)
    }

    fn put(&mut self, key: CacheKey, value: CachedSet) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| *k == key) {
            self.entries.remove(pos);
        } else if self.entries.len() >= self.capacity {
            self.entries.remove(0);
        }
        self.entries.push((key, value));
    }
}

/// The default validator-set cache capacity (Go `cachedState` default).
pub const DEFAULT_CACHE_SIZE: usize = 64;

/// LRU-memoizing [`ValidatorState`] adapter (Go `validators.cachedState`).
pub struct CachedState {
    inner: Arc<dyn ValidatorState>,
    cache: Mutex<Lru>,
}

impl CachedState {
    /// Wraps `inner`, caching up to `capacity` validator sets.
    #[must_use]
    pub fn new(inner: Arc<dyn ValidatorState>, capacity: usize) -> Self {
        Self {
            inner,
            cache: Mutex::new(Lru::new(capacity)),
        }
    }

    /// Wraps `inner` with the [`DEFAULT_CACHE_SIZE`] capacity.
    #[must_use]
    pub fn with_default_size(inner: Arc<dyn ValidatorState>) -> Self {
        Self::new(inner, DEFAULT_CACHE_SIZE)
    }
}

#[async_trait]
impl ValidatorState for CachedState {
    async fn get_minimum_height(&self) -> Result<u64> {
        self.inner.get_minimum_height().await
    }

    async fn get_current_height(&self) -> Result<u64> {
        self.inner.get_current_height().await
    }

    async fn get_subnet_id(&self, chain: Id) -> Result<Id> {
        self.inner.get_subnet_id(chain).await
    }

    async fn get_validator_set(
        &self,
        height: u64,
        subnet: Id,
    ) -> Result<BTreeMap<NodeId, GetValidatorOutput>> {
        let key = (height, subnet);
        if let Ok(mut cache) = self.cache.lock()
            && let Some(hit) = cache.get(key)
        {
            return Ok((*hit).clone());
        }
        let set = self.inner.get_validator_set(height, subnet).await?;
        let shared = Arc::new(set);
        if let Ok(mut cache) = self.cache.lock() {
            cache.put(key, Arc::clone(&shared));
        }
        Ok((*shared).clone())
    }

    async fn get_current_validator_set(
        &self,
        subnet: Id,
    ) -> Result<(BTreeMap<Id, crate::state::GetCurrentValidatorOutput>, u64)> {
        // The current set is uncached (it changes every block), matching Go.
        self.inner.get_current_validator_set(subnet).await
    }

    async fn get_warp_validator_sets(
        &self,
        height: u64,
    ) -> Result<HashMap<Id, crate::state::WarpSet>> {
        self.inner.get_warp_validator_sets(height).await
    }
}

/// Mutex-serializing [`ValidatorState`] adapter (Go `validators.lockedState`).
pub struct LockedState {
    inner: AsyncMutex<Arc<dyn ValidatorState>>,
}

impl LockedState {
    /// Wraps `inner` behind a `tokio::sync::Mutex`.
    #[must_use]
    pub fn new(inner: Arc<dyn ValidatorState>) -> Self {
        Self {
            inner: AsyncMutex::new(inner),
        }
    }
}

#[async_trait]
impl ValidatorState for LockedState {
    async fn get_minimum_height(&self) -> Result<u64> {
        let inner = self.inner.lock().await;
        inner.get_minimum_height().await
    }

    async fn get_current_height(&self) -> Result<u64> {
        let inner = self.inner.lock().await;
        inner.get_current_height().await
    }

    async fn get_subnet_id(&self, chain: Id) -> Result<Id> {
        let inner = self.inner.lock().await;
        inner.get_subnet_id(chain).await
    }

    async fn get_validator_set(
        &self,
        height: u64,
        subnet: Id,
    ) -> Result<BTreeMap<NodeId, GetValidatorOutput>> {
        let inner = self.inner.lock().await;
        inner.get_validator_set(height, subnet).await
    }

    async fn get_current_validator_set(
        &self,
        subnet: Id,
    ) -> Result<(BTreeMap<Id, crate::state::GetCurrentValidatorOutput>, u64)> {
        let inner = self.inner.lock().await;
        inner.get_current_validator_set(subnet).await
    }

    async fn get_warp_validator_sets(
        &self,
        height: u64,
    ) -> Result<HashMap<Id, crate::state::WarpSet>> {
        let inner = self.inner.lock().await;
        inner.get_warp_validator_sets(height).await
    }
}
