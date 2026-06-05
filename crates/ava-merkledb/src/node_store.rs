// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The two on-disk node stores over a base [`ava_database::Database`]:
//! [`ValueNodeDb`] (always durable) and [`IntermediateNodeDb`] (LRU-cached,
//! rebuildable). Byte-exact port of Go `x/merkledb/value_node_db.go` /
//! `intermediate_node_db.go` (spec 04 §3.5, §10.8).
//!
//! Both stores prepend a 1-byte prefix to the trie key (`addPrefixToKey`):
//! - value nodes use `valueNodePrefix` over `Key.bytes()` directly;
//! - intermediate nodes use `intermediateNodePrefix`, and — for sub-byte token
//!   sizes — append a padding token so two keys of equal byte length but
//!   different bit length stay distinct (Go `constructDBKey`).

use std::num::NonZeroUsize;
use std::sync::Arc;

use bytes::Bytes;
use lru::LruCache;
use parking_lot::Mutex;

use ava_database::Database;

use crate::codec::{decode_db_node, encode_db_node};
use crate::error::{Error, Result};
use crate::key::{BranchFactor, Key, bytes_needed};
use crate::node::{DbNode, prefix};

/// Default LRU capacity for the node caches (mirrors Go's defaults loosely; the
/// exact size is not protocol-relevant).
const DEFAULT_CACHE_ENTRIES: usize = 1 << 16;

/// Prepends `prefix` to `key_bytes`. Mirrors Go `addPrefixToKey` (byte-token
/// case). Returns the full base-DB key.
fn add_prefix(prefix: u8, key_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(key_bytes.len().saturating_add(1));
    out.push(prefix);
    out.extend_from_slice(key_bytes);
    out
}

/// The value-node store: durable, never rebuilt. Caches decoded nodes.
pub(crate) struct ValueNodeDb<D: Database> {
    base: Arc<D>,
    cache: Mutex<LruCache<Key, DbNode>>,
}

impl<D: Database> ValueNodeDb<D> {
    pub(crate) fn new(base: Arc<D>) -> Self {
        ValueNodeDb {
            base,
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(DEFAULT_CACHE_ENTRIES).unwrap_or(NonZeroUsize::MIN),
            )),
        }
    }

    /// The full base-DB key for `key` (value-node prefix + packed bytes).
    fn db_key(key: &Key) -> Vec<u8> {
        add_prefix(prefix::VALUE_NODE, key.bytes())
    }

    /// Reads the value node at `key`, or `None` if absent.
    pub(crate) fn get(&self, key: &Key) -> Result<Option<DbNode>> {
        if let Some(n) = self.cache.lock().get(key) {
            return Ok(Some(n.clone()));
        }
        match self.base.get(&Self::db_key(key)) {
            Ok(bytes) => {
                let node = decode_db_node(&bytes)?;
                self.cache.lock().put(key.clone(), node.clone());
                Ok(Some(node))
            }
            Err(ava_database::Error::NotFound) => Ok(None),
            Err(e) => Err(Error::from(e)),
        }
    }

    /// Buffers a put of `node` at `key` onto the value-node batch `ops`.
    pub(crate) fn stage_put(
        &self,
        ops: &mut Vec<(Vec<u8>, Option<Bytes>)>,
        key: &Key,
        node: &DbNode,
    ) {
        self.cache.lock().put(key.clone(), node.clone());
        ops.push((Self::db_key(key), Some(Bytes::from(encode_db_node(node)))));
    }

    /// Buffers a delete of `key` onto the value-node batch `ops`.
    pub(crate) fn stage_delete(&self, ops: &mut Vec<(Vec<u8>, Option<Bytes>)>, key: &Key) {
        self.cache.lock().pop(key);
        ops.push((Self::db_key(key), None));
    }

    /// Returns the base DB key prefix for value-node iteration.
    pub(crate) fn prefix() -> u8 {
        prefix::VALUE_NODE
    }
}

/// The intermediate-node store: LRU-cached, rebuildable from value nodes.
pub(crate) struct IntermediateNodeDb<D: Database> {
    base: Arc<D>,
    cache: Mutex<LruCache<Key, DbNode>>,
    token_size: usize,
}

impl<D: Database> IntermediateNodeDb<D> {
    pub(crate) fn new(base: Arc<D>, branch_factor: BranchFactor) -> Self {
        IntermediateNodeDb {
            base,
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(DEFAULT_CACHE_ENTRIES).unwrap_or(NonZeroUsize::MIN),
            )),
            token_size: branch_factor.token_size(),
        }
    }

    /// Constructs the base-DB key for `key`. For byte tokens this is just the
    /// prefix + packed bytes; for sub-byte tokens a padding token is appended so
    /// equal-byte-length / different-bit-length keys stay distinct (Go
    /// `constructDBKey`).
    fn db_key(&self, key: &Key) -> Vec<u8> {
        if self.token_size == 8 {
            return add_prefix(prefix::INTERMEDIATE_NODE, key.bytes());
        }
        let prefix_len = 1usize;
        let prefix_bit_len = 8usize.saturating_mul(prefix_len);
        // Go: paddingByteValue = 1 << dualBitIndex(tokenSize); stored as a single
        // token. `to_token(1, ts)` left-aligns to exactly that byte.
        let padding_key = Key::to_token(1, self.token_size);

        let total_bits = prefix_bit_len
            .saturating_add(key.length())
            .saturating_add(self.token_size);
        let mut buf = vec![0u8; bytes_needed(total_bits)];
        buf[0] = prefix::INTERMEDIATE_NODE;
        let kb = key.bytes();
        let copy_len = kb.len().min(buf.len().saturating_sub(prefix_len));
        buf[prefix_len..prefix_len + copy_len].copy_from_slice(&kb[..copy_len]);
        crate::key::extend_into_buffer_pub(&mut buf, &padding_key, prefix_bit_len + key.length());
        buf
    }

    /// Reads the intermediate node at `key`, or `None` if absent. Reserved for
    /// proof construction (M1.17).
    #[allow(dead_code)]
    pub(crate) fn get(&self, key: &Key) -> Result<Option<DbNode>> {
        if let Some(n) = self.cache.lock().get(key) {
            return Ok(Some(n.clone()));
        }
        match self.base.get(&self.db_key(key)) {
            Ok(bytes) => {
                let node = decode_db_node(&bytes)?;
                self.cache.lock().put(key.clone(), node.clone());
                Ok(Some(node))
            }
            Err(ava_database::Error::NotFound) => Ok(None),
            Err(e) => Err(Error::from(e)),
        }
    }

    /// Writes `node` at `key` (cache + base DB).
    pub(crate) fn put(&self, key: &Key, node: &DbNode) -> Result<()> {
        self.cache.lock().put(key.clone(), node.clone());
        self.base.put(&self.db_key(key), &encode_db_node(node))?;
        Ok(())
    }

    /// Deletes `key` (cache + base DB).
    pub(crate) fn delete(&self, key: &Key) -> Result<()> {
        self.cache.lock().pop(key);
        self.base.delete(&self.db_key(key))?;
        Ok(())
    }

    /// Returns the base DB key prefix for intermediate-node iteration.
    pub(crate) fn prefix() -> u8 {
        prefix::INTERMEDIATE_NODE
    }
}
