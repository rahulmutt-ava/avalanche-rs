// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The atomic [`Memory`] + per-chain [`SharedMemoryView`] implementation of the
//! [`SharedMemory`] trait (`chains/atomic`, specs 07 §3.1).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ava_codec::packer::Packer;
use ava_crypto::hashing::sha256;
use ava_database::prefixdb::{join_prefixes, make_prefix};
use ava_database::{Batch, DynDatabase};
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{
    Element, IndexedResult, Requests, SharedMemory,
};

use crate::error::{Error, Result};

/// The codec version every atomic value is framed with (Go `atomic.CodecVersion`).
const CODEC_VERSION: u16 = 0;

// The inbound value/index prefixes. Outbound swaps smaller<->larger so the two
// chains agree on which half is which (Go `chains/atomic/prefixes.go`).
const INBOUND_SMALLER_VALUE: &[u8] = &[0];
const INBOUND_SMALLER_INDEX: &[u8] = &[1];
const INBOUND_LARGER_VALUE: &[u8] = &[2];
const INBOUND_LARGER_INDEX: &[u8] = &[3];

/// `chains/atomic.Memory` — owns the shared base DB and hands out per-chain
/// [`SharedMemoryView`]s.
pub struct Memory {
    db: Arc<dyn DynDatabase>,
    /// Per-`sharedID` lock so two chains' applies to the same channel serialize
    /// (Go `rcLock`). A single mutex map is sufficient for the synchronous API.
    locks: Mutex<BTreeMap<[u8; 32], Arc<Mutex<()>>>>,
}

impl Memory {
    /// `NewMemory(db)` — builds the shared-memory owner over a base DB.
    #[must_use]
    pub fn new(db: Arc<dyn DynDatabase>) -> Arc<Self> {
        Arc::new(Self {
            db,
            locks: Mutex::new(BTreeMap::new()),
        })
    }

    /// `NewSharedMemory(chainID)` — the view chain `chain_id` operates through.
    #[must_use]
    pub fn new_shared_memory(self: &Arc<Self>, chain_id: Id) -> SharedMemoryView {
        SharedMemoryView {
            memory: Arc::clone(self),
            this_chain: chain_id,
        }
    }

    fn lock_for(&self, shared: [u8; 32]) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(locks.entry(shared).or_insert_with(|| Arc::new(Mutex::new(()))))
    }
}

/// A single chain's view of cross-chain atomic storage.
pub struct SharedMemoryView {
    memory: Arc<Memory>,
    this_chain: Id,
}

/// The four key namespaces for a `(this, peer)` channel, already reduced to the
/// concrete value/index prefix bytes for this chain's perspective.
struct Prefixes {
    value: &'static [u8],
    index: &'static [u8],
}

impl SharedMemoryView {
    /// Whether this chain is the lexically-smaller of the pair.
    fn is_smaller(&self, peer: Id) -> bool {
        self.this_chain.to_bytes() < peer.to_bytes()
    }

    /// The inbound (read) value/index prefixes for this chain.
    fn inbound(&self, peer: Id) -> Prefixes {
        if self.is_smaller(peer) {
            Prefixes {
                value: INBOUND_SMALLER_VALUE,
                index: INBOUND_SMALLER_INDEX,
            }
        } else {
            Prefixes {
                value: INBOUND_LARGER_VALUE,
                index: INBOUND_LARGER_INDEX,
            }
        }
    }

    /// The outbound (write) value/index prefixes for this chain (the peer's
    /// inbound half).
    fn outbound(&self, peer: Id) -> Prefixes {
        if self.is_smaller(peer) {
            Prefixes {
                value: INBOUND_LARGER_VALUE,
                index: INBOUND_LARGER_INDEX,
            }
        } else {
            Prefixes {
                value: INBOUND_SMALLER_VALUE,
                index: INBOUND_SMALLER_INDEX,
            }
        }
    }

    /// The 32-byte namespace prefix for the `(this, peer)` channel value DB
    /// under `prefix`: `JoinPrefixes(MakePrefix(sharedID), prefix)`.
    fn ns(&self, peer: Id, prefix: &[u8]) -> Vec<u8> {
        let shared = shared_id(self.this_chain, peer);
        join_prefixes(&make_prefix(&shared), prefix)
    }

    /// The fully-prefixed value-DB key for `key` in namespace `prefix`.
    fn value_key(&self, ns: &[u8], key: &[u8]) -> Vec<u8> {
        let mut full = Vec::with_capacity(ns.len().saturating_add(key.len()));
        full.extend_from_slice(ns);
        full.extend_from_slice(key);
        full
    }

    /// The index key for `(trait, key)` under index namespace `ns`:
    /// `ns ‖ len(trait) as u32-be ‖ trait ‖ key`. The length prefix keeps the
    /// trait/key boundary unambiguous so `Indexed` can split it back out.
    fn index_key(&self, ns: &[u8], r#trait: &[u8], key: &[u8]) -> Vec<u8> {
        let mut full = Vec::with_capacity(
            ns.len()
                .saturating_add(4)
                .saturating_add(r#trait.len())
                .saturating_add(key.len()),
        );
        full.extend_from_slice(ns);
        full.extend_from_slice(&(r#trait.len() as u32).to_be_bytes());
        full.extend_from_slice(r#trait);
        full.extend_from_slice(key);
        full
    }
}

impl SharedMemory for SharedMemoryView {
    fn get(&self, peer_chain: Id, keys: &[Vec<u8>]) -> ava_vm::error::Result<Vec<Vec<u8>>> {
        let p = self.inbound(peer_chain);
        let ns = self.ns(peer_chain, p.value);
        let mut values = Vec::with_capacity(keys.len());
        for key in keys {
            let full = self.value_key(&ns, key);
            let raw = self
                .memory
                .db
                .get(&full)
                .map_err(map_db_err)?;
            let elem = decode_db_element(&raw).map_err(map_err)?;
            if !elem.present {
                // Indexed but tombstoned ⇒ NotFound (Go `state.Value`).
                return Err(ava_vm::error::Error::NotFound);
            }
            values.push(elem.value);
        }
        Ok(values)
    }

    fn indexed(
        &self,
        peer_chain: Id,
        traits: &[Vec<u8>],
        _start_trait: &[u8],
        start_key: &[u8],
        limit: usize,
    ) -> ava_vm::error::Result<IndexedResult> {
        let p = self.inbound(peer_chain);
        let index_ns = self.ns(peer_chain, p.index);
        let value_ns = self.ns(peer_chain, p.value);

        let mut values = Vec::new();
        let mut last_trait = Vec::new();
        let mut last_key = Vec::new();

        'outer: for r#trait in traits {
            let trait_prefix = self.index_key(&index_ns, r#trait, &[]);
            let mut iter = self
                .memory
                .db
                .new_iterator_with_start_and_prefix(start_key, &trait_prefix);
            while iter.next() {
                if values.len() >= limit {
                    break 'outer;
                }
                let Some(index_key) = iter.key() else {
                    continue;
                };
                let index_key = index_key.to_vec();
                // The element key is the suffix after `ns ‖ len ‖ trait`.
                let key = index_key
                    .get(trait_prefix.len()..)
                    .unwrap_or_default()
                    .to_vec();
                let value_full = self.value_key(&value_ns, &key);
                let raw = self.memory.db.get(&value_full).map_err(map_db_err)?;
                let elem = decode_db_element(&raw).map_err(map_err)?;
                if elem.present {
                    values.push(elem.value);
                    last_trait = r#trait.clone();
                    last_key = key;
                }
            }
        }
        Ok((values, last_trait, last_key))
    }

    fn apply(
        &self,
        requests: BTreeMap<Id, Requests>,
        batches: &[ava_database::BatchOps],
    ) -> ava_vm::error::Result<()> {
        self.apply_inner(requests, batches).map_err(map_err)
    }
}

impl SharedMemoryView {
    /// The crate-error apply path; the trait method maps it into `ava_vm::Error`.
    fn apply_inner(
        &self,
        requests: BTreeMap<Id, Requests>,
        batches: &[ava_database::BatchOps],
    ) -> Result<()> {
        // Lock every touched channel in sorted sharedID order (deadlock-free).
        let mut shared_ids: Vec<[u8; 32]> = requests
            .keys()
            .map(|peer| shared_id(self.this_chain, *peer))
            .collect();
        shared_ids.sort_unstable();
        let guards: Vec<Arc<Mutex<()>>> =
            shared_ids.iter().map(|s| self.memory.lock_for(*s)).collect();
        let _held: Vec<_> = guards
            .iter()
            .map(|g| g.lock().unwrap_or_else(|e| e.into_inner()))
            .collect();

        // Accumulate every value/index op into one base batch, then replay the
        // caller's side batches into the same batch and write once — so the
        // atomic state and the chain's own state commit together (Go: a
        // versiondb whose CommitBatch is WriteAll'd with the side batches).
        let mut batch = self.memory.db.new_batch();

        for (peer, req) in &requests {
            // Removes hit this chain's INBOUND half (consuming a received UTXO).
            let in_p = self.inbound(*peer);
            let in_value_ns = self.ns(*peer, in_p.value);
            let in_index_ns = self.ns(*peer, in_p.index);
            for key in &req.remove {
                self.remove_value(batch.as_mut(), &in_value_ns, &in_index_ns, key)?;
            }

            // Puts hit this chain's OUTBOUND half (sending to the peer).
            let out_p = self.outbound(*peer);
            let out_value_ns = self.ns(*peer, out_p.value);
            let out_index_ns = self.ns(*peer, out_p.index);
            for elem in &req.put {
                self.set_value(batch.as_mut(), &out_value_ns, &out_index_ns, elem)?;
            }
        }

        // Merge the caller's side batches into the same atomic write.
        for side in batches {
            for op in &side.ops {
                if op.delete {
                    batch.delete(&op.key)?;
                } else {
                    batch.put(&op.key, &op.value)?;
                }
            }
        }

        batch.write()?;
        Ok(())
    }

    /// Writes `elem` into the value DB + indexes each trait. A previously
    /// tombstoned key cancels out (the put + the tombstone delete each other).
    fn set_value(
        &self,
        batch: &mut dyn Batch,
        value_ns: &[u8],
        index_ns: &[u8],
        elem: &Element,
    ) -> Result<()> {
        let value_key = self.value_key(value_ns, &elem.key);
        if let Ok(raw) = self.memory.db.get(&value_key) {
            let existing = decode_db_element(&raw)?;
            if !existing.present {
                // Optimistically-deleted earlier ⇒ cancel out (Go `SetValue`).
                batch.delete(&value_key)?;
                return Ok(());
            }
            return Err(Error::DuplicateAtomicOp);
        }
        for r#trait in &elem.traits {
            let index_key = self.index_key(index_ns, r#trait, &elem.key);
            batch.put(&index_key, &[])?;
        }
        let encoded = encode_db_element(true, &elem.value, &elem.traits);
        batch.put(&value_key, &encoded)?;
        Ok(())
    }

    /// Removes `key` from the value DB + its trait indexes. If the key is absent
    /// it is tombstoned (Present=false) so a later add cancels (Go `RemoveValue`).
    fn remove_value(
        &self,
        batch: &mut dyn Batch,
        value_ns: &[u8],
        index_ns: &[u8],
        key: &[u8],
    ) -> Result<()> {
        let value_key = self.value_key(value_ns, key);
        match self.memory.db.get(&value_key) {
            Err(ava_database::Error::NotFound) => {
                // Tombstone for a not-yet-added element.
                let encoded = encode_db_element(false, &[], &[]);
                batch.put(&value_key, &encoded)?;
                Ok(())
            }
            Err(e) => Err(Error::Database(e)),
            Ok(raw) => {
                let existing = decode_db_element(&raw)?;
                if !existing.present {
                    return Err(Error::DuplicateAtomicOp);
                }
                for r#trait in &existing.traits {
                    let index_key = self.index_key(index_ns, r#trait, key);
                    batch.delete(&index_key)?;
                }
                batch.delete(&value_key)?;
                Ok(())
            }
        }
    }
}

/// `sharedID(id1, id2)` — `ComputeHash256(Codec.Marshal([2]ids.ID{min, max}))`
/// where the marshalling is the 2-byte version prefix followed by the two
/// 32-byte ids (a fixed-size array has no length prefix).
fn shared_id(a: Id, b: Id) -> [u8; 32] {
    let (lo, hi) = if a.to_bytes() <= b.to_bytes() {
        (a, b)
    } else {
        (b, a)
    };
    let mut buf = Vec::with_capacity(2 + 64);
    buf.extend_from_slice(&CODEC_VERSION.to_be_bytes());
    buf.extend_from_slice(&lo.to_bytes());
    buf.extend_from_slice(&hi.to_bytes());
    sha256(&buf)
}

/// A decoded `dbElement` (`{Present, Value, Traits}`).
struct DbElement {
    present: bool,
    value: Vec<u8>,
    #[allow(dead_code)]
    traits: Vec<Vec<u8>>,
}

/// Encodes a `dbElement` byte-exactly: 2-byte version prefix + `bool` +
/// length-prefixed value + `u32` trait count + each length-prefixed trait
/// (linear-codec layout).
fn encode_db_element(present: bool, value: &[u8], traits: &[Vec<u8>]) -> Vec<u8> {
    // version(2) + present(1) + value-len(4) + value + trait-count(4) +
    // per-trait (len(4) + bytes). `new_write` caps the buffer at the hint, so it
    // must be the full encoded size.
    let traits_len: usize = traits
        .iter()
        .map(|t| 4usize.saturating_add(t.len()))
        .fold(0usize, |acc, n| acc.saturating_add(n));
    let cap = 2usize
        .saturating_add(1)
        .saturating_add(4)
        .saturating_add(value.len())
        .saturating_add(4)
        .saturating_add(traits_len);
    let mut p = Packer::new_write(cap);
    p.pack_u16(CODEC_VERSION);
    p.pack_bool(present);
    p.pack_bytes(value);
    p.pack_u32(traits.len() as u32);
    for r#trait in traits {
        p.pack_bytes(r#trait);
    }
    p.into_bytes()
}

/// Decodes a `dbElement` produced by [`encode_db_element`].
fn decode_db_element(raw: &[u8]) -> Result<DbElement> {
    let mut p = Packer::new_read(raw);
    let version = p.unpack_u16();
    if version != CODEC_VERSION {
        return Err(Error::Other(format!(
            "unexpected atomic codec version {version}"
        )));
    }
    let present = p.unpack_bool();
    let value = p.unpack_bytes();
    let count = p.unpack_u32();
    let mut traits = Vec::with_capacity(count as usize);
    for _ in 0..count {
        traits.push(p.unpack_bytes());
    }
    if p.errored() {
        return Err(Error::Other("malformed atomic dbElement".to_string()));
    }
    Ok(DbElement {
        present,
        value,
        traits,
    })
}

fn map_db_err(e: ava_database::Error) -> ava_vm::error::Error {
    match e {
        ava_database::Error::NotFound => ava_vm::error::Error::NotFound,
        // `ava_vm::Error` is a closed sentinel enum (specs 07 §9) with no
        // free-form variant; an unexpected atomic-storage fault surfaces as an
        // invalid-component error naming the source.
        _ => ava_vm::error::Error::InvalidComponent("atomic shared memory"),
    }
}

fn map_err(e: Error) -> ava_vm::error::Error {
    match e {
        Error::NotFound => ava_vm::error::Error::NotFound,
        Error::Database(ava_database::Error::NotFound) => ava_vm::error::Error::NotFound,
        _ => ava_vm::error::Error::InvalidComponent("atomic shared memory"),
    }
}
