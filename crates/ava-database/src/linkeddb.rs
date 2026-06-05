// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `linkeddb` — an in-DB doubly-linked list giving a non-iterating KV store LIFO
//! iteration (04 §2.7, §10.6), mirroring `database/linkeddb/linkeddb.go`.
//!
//! Each logical entry is stored under `node_key(key) = 0x00 ‖ key` as a
//! linearcodec-encoded [`Node`] carrying `value / has_next / next /
//! has_previous / previous`; a head pointer lives at the fixed key
//! [`HEAD_KEY`] = `0x01`. **These node bytes are persisted**, so the codec is
//! byte-exact with Go (guarded by `tests/golden_linkeddb.rs`) — a list migrated
//! from a Go node iterates identically.
//!
//! Inserts prepend at the head (so iteration is LIFO / most-recent-first). The
//! head key and decoded nodes are cached in bounded LRUs; a per-mutation
//! `updated_nodes` staging map plus a single batch write of the head + touched
//! nodes mirror Go's `resetBatch`/`writeBatch`. Used by P-Chain list spaces
//! (e.g. pending-staker / subnet / chain sets — see §10).

use std::num::NonZeroUsize;
use std::sync::Arc;

use ava_codec::linearcodec::LinearCodec;
use ava_codec::manager::Manager;
use ava_codec::{AvaCodec, Deserializable, Serializable};
use lru::LruCache;
use parking_lot::Mutex;

use crate::error::{Error, Result};
use crate::traits::{Batch, Database, Iterator};

/// The codec version framing persisted node bytes (`linkeddb.CodecVersion`).
pub const CODEC_VERSION: u16 = 0;

/// The fixed key under which the list's head pointer is stored
/// (`linkeddb.headKey`).
pub const HEAD_KEY: &[u8] = &[0x01];

/// The default node/head LRU cache size (`linkeddb.defaultCacheSize`).
pub const DEFAULT_CACHE_SIZE: usize = 1024;

/// A doubly-linked-list node, serialized with the linearcodec exactly as Go's
/// `linkeddb.node` (fields in declared order; **these bytes are persisted**).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    /// The stored value for this entry.
    #[codec]
    pub value: Vec<u8>,
    /// Whether a next (older) node exists.
    #[codec]
    pub has_next: bool,
    /// The key of the next (older) node, when [`Node::has_next`].
    #[codec]
    pub next: Vec<u8>,
    /// Whether a previous (newer) node exists.
    #[codec]
    pub has_previous: bool,
    /// The key of the previous (newer) node, when [`Node::has_previous`].
    #[codec]
    pub previous: Vec<u8>,
}

/// Builds the shared linkeddb codec manager (linear codec at [`CODEC_VERSION`]).
fn codec_manager() -> Manager {
    let m = Manager::new(i32::MAX as usize);
    // Registration cannot fail for a single fresh version.
    let _ = m.register(CODEC_VERSION, Arc::new(LinearCodec::new()));
    m
}

/// `node_key(key) = 0x00 ‖ key` — the on-disk key for a node (`linkeddb.nodeKey`).
#[must_use]
pub fn node_key(key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(key.len().saturating_add(1));
    out.push(0x00);
    out.extend_from_slice(key);
    out
}

/// Encodes `node` to its byte-exact persisted form (incl. the 2-byte version
/// prefix), mirroring `linkeddb.Codec.Marshal(CodecVersion, node)`.
pub fn encode_node(node: &Node) -> Result<Vec<u8>> {
    codec_manager()
        .marshal(CODEC_VERSION, node as &dyn Serializable)
        .map_err(|e| Error::Other(anyhow::anyhow!("{e}")))
}

/// Decodes a [`Node`] from its persisted bytes (`linkeddb.Codec.Unmarshal`).
pub fn decode_node(bytes: &[u8]) -> Result<Node> {
    let mut node = Node::default();
    codec_manager()
        .unmarshal(bytes, &mut node as &mut dyn Deserializable)
        .map_err(|e| Error::Other(anyhow::anyhow!("{e}")))?;
    Ok(node)
}

/// An in-DB doubly-linked list over a base [`Database`] (04 §2.7).
///
/// The base is held behind an [`Arc`] so the linkeddb can outlive the caller's
/// borrow and share a base namespaced by a `prefixdb` (the common §10 layout:
/// a linkeddb's nodes live *inside* an already-namespaced prefix).
pub struct LinkedDb<D: Database> {
    db: Arc<D>,
    state: Mutex<State>,
}

/// Mutable cache/state, guarded by a single mutex (Go uses an `RWMutex` +
/// `cacheLock`; one mutex is simpler and equivalent for our synchronous use).
struct State {
    /// Cached head key. `head_synced` gates whether `head` is authoritative.
    head_synced: bool,
    head_exists: bool,
    head: Vec<u8>,
    /// Decoded-node LRU: `key → Some(node)` present, `None` ⇒ known-absent.
    node_cache: LruCache<Vec<u8>, Option<Node>>,
}

impl<D: Database> LinkedDb<D> {
    /// Wraps `db` with the default cache size.
    pub fn new(db: D) -> Self {
        Self::with_cache_size(db, DEFAULT_CACHE_SIZE)
    }

    /// Wraps an already-`Arc`'d base (so a namespaced base can be shared).
    pub fn new_arc(db: Arc<D>) -> Self {
        Self::with_cache_size_arc(db, DEFAULT_CACHE_SIZE)
    }

    /// Wraps `db` with an explicit node/head cache size.
    pub fn with_cache_size(db: D, cache_size: usize) -> Self {
        Self::with_cache_size_arc(Arc::new(db), cache_size)
    }

    fn with_cache_size_arc(db: Arc<D>, cache_size: usize) -> Self {
        let cap = NonZeroUsize::new(cache_size.max(1)).unwrap_or(NonZeroUsize::MIN);
        Self {
            db,
            state: Mutex::new(State {
                head_synced: false,
                head_exists: false,
                head: Vec::new(),
                node_cache: LruCache::new(cap),
            }),
        }
    }

    /// Whether the list is empty (no head).
    pub fn is_empty(&self) -> Result<bool> {
        match self.head_key() {
            Ok(_) => Ok(false),
            Err(Error::NotFound) => Ok(true),
            Err(e) => Err(e),
        }
    }

    /// Returns the current head key, or [`Error::NotFound`] when empty.
    pub fn head_key(&self) -> Result<Vec<u8>> {
        let mut state = self.state.lock();
        self.get_head_key(&mut state)
    }

    /// Returns the head `(key, value)`, or [`Error::NotFound`] when empty.
    pub fn head(&self) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut state = self.state.lock();
        let head = self.get_head_key(&mut state)?;
        let node = self.get_node(&mut state, &head)?;
        Ok((head, node.value))
    }

    /// Stores `value` under `key`, prepending a new node at the head if `key` is
    /// not already in the list (matching Go's `Put`).
    pub fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let mut state = self.state.lock();
        let mut batch = self.db.new_batch();
        let mut staged: Vec<(Vec<u8>, Option<Node>)> = Vec::new();
        let mut staged_head: Option<(bool, Vec<u8>)> = None;

        // If the key already has a node, just update its value in place.
        match self.get_node(&mut state, key) {
            Ok(mut existing) => {
                existing.value = value.to_vec();
                self.put_node(&mut batch, &mut staged, key, existing)?;
                return self.write_batch(&mut state, &mut batch, staged, staged_head);
            }
            Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }

        // Add the key as the new head.
        let mut new_head = Node {
            value: value.to_vec(),
            ..Node::default()
        };
        match self.get_head_key(&mut state) {
            Ok(old_head_key) => {
                let mut old_head = self.get_node(&mut state, &old_head_key)?;
                old_head.has_previous = true;
                old_head.previous = key.to_vec();
                self.put_node(&mut batch, &mut staged, &old_head_key, old_head)?;

                new_head.has_next = true;
                new_head.next = old_head_key;
            }
            Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }
        self.put_node(&mut batch, &mut staged, key, new_head)?;
        self.put_head_key(&mut batch, &mut staged_head, key)?;
        self.write_batch(&mut state, &mut batch, staged, staged_head)
    }

    /// Removes `key` from the list, relinking neighbors (matching Go's `Delete`).
    pub fn delete(&self, key: &[u8]) -> Result<()> {
        let mut state = self.state.lock();
        let current = match self.get_node(&mut state, key) {
            Ok(n) => n,
            Err(Error::NotFound) => return Ok(()),
            Err(e) => return Err(e),
        };

        let mut batch = self.db.new_batch();
        let mut staged: Vec<(Vec<u8>, Option<Node>)> = Vec::new();
        let mut staged_head: Option<(bool, Vec<u8>)> = None;

        self.delete_node(&mut batch, &mut staged, key)?;

        if current.has_previous {
            // Not the head: relink previous → next.
            let mut previous = self.get_node(&mut state, &current.previous)?;
            previous.has_next = current.has_next;
            previous.next = current.next.clone();
            self.put_node(&mut batch, &mut staged, &current.previous, previous)?;
            if current.has_next {
                let mut next = self.get_node(&mut state, &current.next)?;
                next.has_previous = true;
                next.previous = current.previous.clone();
                self.put_node(&mut batch, &mut staged, &current.next, next)?;
            }
        } else if !current.has_next {
            // Only node: the list no longer has a head.
            self.delete_head_key(&mut batch, &mut staged_head)?;
        } else {
            // Head with a successor: the next node becomes the new head.
            self.put_head_key(&mut batch, &mut staged_head, &current.next)?;
            let mut next = self.get_node(&mut state, &current.next)?;
            next.has_previous = false;
            next.previous = Vec::new();
            self.put_node(&mut batch, &mut staged, &current.next, next)?;
        }
        self.write_batch(&mut state, &mut batch, staged, staged_head)
    }

    /// Returns whether `key` is in the list.
    pub fn has(&self, key: &[u8]) -> Result<bool> {
        self.db.has(&node_key(key))
    }

    /// Returns the value for `key`, or [`Error::NotFound`] when absent.
    pub fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        let mut state = self.state.lock();
        Ok(self.get_node(&mut state, key)?.value)
    }

    /// A LIFO iterator from the head (most-recently-inserted first).
    pub fn new_iterator(&self) -> LinkedIterator<'_, D> {
        LinkedIterator {
            ldb: self,
            initialized: false,
            exhausted: false,
            key: None,
            value: None,
            next_key: Vec::new(),
            err: None,
        }
    }

    /// A LIFO iterator starting at `start` (or the head if `start` is absent),
    /// mirroring Go's `NewIteratorWithStart`.
    pub fn new_iterator_with_start(&self, start: &[u8]) -> LinkedIterator<'_, D> {
        if matches!(self.has(start), Ok(true)) {
            return LinkedIterator {
                ldb: self,
                initialized: true,
                exhausted: false,
                key: None,
                value: None,
                next_key: start.to_vec(),
                err: None,
            };
        }
        self.new_iterator()
    }

    // --- internal node/head accessors (operate on the locked state) ----------

    fn get_head_key(&self, state: &mut State) -> Result<Vec<u8>> {
        if state.head_synced {
            if state.head_exists {
                return Ok(state.head.clone());
            }
            return Err(Error::NotFound);
        }
        match self.db.get(HEAD_KEY) {
            Ok(head) => {
                state.head_synced = true;
                state.head_exists = true;
                state.head = head.clone();
                Ok(head)
            }
            Err(Error::NotFound) => {
                state.head_synced = true;
                state.head_exists = false;
                Err(Error::NotFound)
            }
            Err(e) => Err(e),
        }
    }

    fn get_node(&self, state: &mut State, key: &[u8]) -> Result<Node> {
        if let Some(cached) = state.node_cache.get(key) {
            return match cached {
                Some(n) => Ok(n.clone()),
                None => Err(Error::NotFound),
            };
        }
        match self.db.get(&node_key(key)) {
            Ok(bytes) => {
                let node = decode_node(&bytes)?;
                state.node_cache.put(key.to_vec(), Some(node.clone()));
                Ok(node)
            }
            Err(Error::NotFound) => {
                state.node_cache.put(key.to_vec(), None);
                Err(Error::NotFound)
            }
            Err(e) => Err(e),
        }
    }

    fn put_node(
        &self,
        batch: &mut Box<dyn Batch + '_>,
        staged: &mut Vec<(Vec<u8>, Option<Node>)>,
        key: &[u8],
        node: Node,
    ) -> Result<()> {
        let bytes = encode_node(&node)?;
        staged.push((key.to_vec(), Some(node)));
        batch.put(&node_key(key), &bytes)
    }

    fn delete_node(
        &self,
        batch: &mut Box<dyn Batch + '_>,
        staged: &mut Vec<(Vec<u8>, Option<Node>)>,
        key: &[u8],
    ) -> Result<()> {
        staged.push((key.to_vec(), None));
        batch.delete(&node_key(key))
    }

    fn put_head_key(
        &self,
        batch: &mut Box<dyn Batch + '_>,
        staged_head: &mut Option<(bool, Vec<u8>)>,
        key: &[u8],
    ) -> Result<()> {
        *staged_head = Some((true, key.to_vec()));
        batch.put(HEAD_KEY, key)
    }

    fn delete_head_key(
        &self,
        batch: &mut Box<dyn Batch + '_>,
        staged_head: &mut Option<(bool, Vec<u8>)>,
    ) -> Result<()> {
        *staged_head = Some((false, Vec::new()));
        batch.delete(HEAD_KEY)
    }

    /// Writes the staged head + nodes atomically, then commits the staged values
    /// into the caches (mirrors Go's `writeBatch`).
    fn write_batch(
        &self,
        state: &mut State,
        batch: &mut Box<dyn Batch + '_>,
        staged: Vec<(Vec<u8>, Option<Node>)>,
        staged_head: Option<(bool, Vec<u8>)>,
    ) -> Result<()> {
        batch.write()?;
        if let Some((exists, head)) = staged_head {
            state.head_synced = true;
            state.head_exists = exists;
            state.head = head;
        }
        for (key, node) in staged {
            state.node_cache.put(key, node);
        }
        Ok(())
    }
}

/// A LIFO cursor over a [`LinkedDb`] (matching Go's `linkeddb.iterator`). Keys
/// are **not** returned in lexicographic order.
pub struct LinkedIterator<'a, D: Database> {
    ldb: &'a LinkedDb<D>,
    initialized: bool,
    exhausted: bool,
    key: Option<Vec<u8>>,
    value: Option<Vec<u8>>,
    next_key: Vec<u8>,
    err: Option<Error>,
}

impl<D: Database> Iterator for LinkedIterator<'_, D> {
    fn next(&mut self) -> bool {
        if self.exhausted {
            self.key = None;
            self.value = None;
            return false;
        }

        let mut state = self.ldb.state.lock();

        if !self.initialized {
            self.initialized = true;
            match self.ldb.get_head_key(&mut state) {
                Ok(head) => self.next_key = head,
                Err(Error::NotFound) => {
                    self.exhausted = true;
                    self.key = None;
                    self.value = None;
                    return false;
                }
                Err(e) => {
                    self.exhausted = true;
                    self.key = None;
                    self.value = None;
                    self.err = Some(e);
                    return false;
                }
            }
        }

        match self.ldb.get_node(&mut state, &self.next_key) {
            Ok(node) => {
                self.key = Some(self.next_key.clone());
                self.value = Some(node.value);
                self.next_key = node.next;
                self.exhausted = !node.has_next;
                true
            }
            Err(Error::NotFound) => {
                self.exhausted = true;
                self.key = None;
                self.value = None;
                false
            }
            Err(e) => {
                self.exhausted = true;
                self.key = None;
                self.value = None;
                self.err = Some(e);
                false
            }
        }
    }

    fn error(&self) -> Result<()> {
        match &self.err {
            None => Ok(()),
            Some(Error::Closed) => Err(Error::Closed),
            Some(Error::NotFound) => Err(Error::NotFound),
            Some(Error::Other(e)) => Err(Error::Other(anyhow::anyhow!("{e}"))),
        }
    }

    fn key(&self) -> Option<&[u8]> {
        self.key.as_deref()
    }

    fn value(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemDb;

    #[test]
    fn node_codec_empty_is_sixteen_bytes() {
        // version(2) + valueLen(4) + hasNext(1) + nextLen(4) + hasPrev(1) +
        // prevLen(4) = 16 bytes, all zero.
        let got = encode_node(&Node::default()).unwrap();
        assert_eq!(hex::encode(&got), "00000000000000000000000000000000");
        assert_eq!(decode_node(&got).unwrap(), Node::default());
    }

    #[test]
    fn lifo_iteration_and_relink() {
        let ldb = LinkedDb::new(MemDb::new());
        assert!(ldb.is_empty().unwrap());

        // Insert a, b, c — head becomes c (most recent).
        ldb.put(b"a", b"1").unwrap();
        ldb.put(b"b", b"2").unwrap();
        ldb.put(b"c", b"3").unwrap();
        assert!(!ldb.is_empty().unwrap());
        assert_eq!(ldb.head_key().unwrap(), b"c");

        // LIFO order: c, b, a.
        let mut keys = Vec::new();
        let mut it = ldb.new_iterator();
        while it.next() {
            keys.push(it.key().unwrap().to_vec());
        }
        it.error().unwrap();
        assert_eq!(keys, vec![b"c".to_vec(), b"b".to_vec(), b"a".to_vec()]);

        // Get / has.
        assert_eq!(ldb.get(b"b").unwrap(), b"2");
        assert!(ldb.has(b"a").unwrap());
        assert!(matches!(ldb.get(b"zzz"), Err(Error::NotFound)));

        // Delete the middle node; list relinks to c, a.
        ldb.delete(b"b").unwrap();
        assert!(matches!(ldb.get(b"b"), Err(Error::NotFound)));
        let mut keys = Vec::new();
        let mut it = ldb.new_iterator();
        while it.next() {
            keys.push(it.key().unwrap().to_vec());
        }
        it.error().unwrap();
        assert_eq!(keys, vec![b"c".to_vec(), b"a".to_vec()]);

        // Delete the head (c); a remains as the sole node and new head.
        ldb.delete(b"c").unwrap();
        assert_eq!(ldb.head_key().unwrap(), b"a");

        // Delete the last node; the list is empty again.
        ldb.delete(b"a").unwrap();
        assert!(ldb.is_empty().unwrap());
        assert!(matches!(ldb.head_key(), Err(Error::NotFound)));
    }

    #[test]
    fn overwrite_value_keeps_position() {
        let ldb = LinkedDb::new(MemDb::new());
        ldb.put(b"a", b"1").unwrap();
        ldb.put(b"b", b"2").unwrap();
        // Overwrite a's value; it must stay the tail, not move to the head.
        ldb.put(b"a", b"updated").unwrap();
        assert_eq!(ldb.get(b"a").unwrap(), b"updated");
        assert_eq!(ldb.head_key().unwrap(), b"b");

        let mut keys = Vec::new();
        let mut it = ldb.new_iterator();
        while it.next() {
            keys.push(it.key().unwrap().to_vec());
        }
        assert_eq!(keys, vec![b"b".to_vec(), b"a".to_vec()]);
    }

    #[test]
    fn iterator_with_start() {
        let ldb = LinkedDb::new(MemDb::new());
        for k in [b"a".as_slice(), b"b", b"c"] {
            ldb.put(k, b"v").unwrap();
        }
        // Start at b → b, a (following `next` toward older entries).
        let mut keys = Vec::new();
        let mut it = ldb.new_iterator_with_start(b"b");
        while it.next() {
            keys.push(it.key().unwrap().to_vec());
        }
        assert_eq!(keys, vec![b"b".to_vec(), b"a".to_vec()]);

        // An absent start falls back to the head.
        let mut it = ldb.new_iterator_with_start(b"absent");
        assert!(it.next());
        assert_eq!(it.key().unwrap(), b"c");
    }
}
