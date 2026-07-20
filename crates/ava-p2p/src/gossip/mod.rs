// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Gossip framework traits (Go `network/p2p/gossip/{gossip.go,set.go}`).
//!
//! This module only carries the shared vocabulary — [`Gossipable`],
//! [`Marshaller`], and [`Set`] — plus the concrete [`bloom::BloomSet`]
//! filter used to answer "have I already seen this id" without shipping the
//! full working set over the wire. The push/pull gossip drivers
//! (Go `PushGossiper`/`PullGossiper` in `gossip.go`) are out of scope for
//! this task; a concrete `Set` implementation (backed by the C-Chain tx pool)
//! lands in a later task.
//!
//! Trait signatures are written to stay object-safe (`Box<dyn Set<T>>`) —
//! see each trait's doc for the Go method it mirrors.

pub mod bloom;

use ava_types::id::Id;

use crate::error::Result;

/// An item that can be gossiped across the network (Go `gossip.Gossipable`).
pub trait Gossipable: Send + Sync {
    /// Returns the id used to deduplicate this item across gossip rounds
    /// (Go `Gossipable.GossipID`).
    fn gossip_id(&self) -> Id;
}

/// Parsing logic for a concrete [`Gossipable`] type (Go `gossip.Marshaller`).
pub trait Marshaller<T>: Send + Sync {
    /// Serializes `t` to its wire representation (Go `MarshalGossip`).
    ///
    /// # Errors
    /// Returns an error if `t` could not be serialized.
    fn marshal(&self, t: &T) -> Result<Vec<u8>>;

    /// Deserializes `bytes` into a `T` (Go `UnmarshalGossip`).
    ///
    /// # Errors
    /// Returns an error if `bytes` is not a valid encoding of `T`.
    fn unmarshal(&self, bytes: &[u8]) -> Result<T>;
}

/// A set of known gossipable items that also exposes a compact bloom-filter
/// summary of its contents (Go `gossip.Set`, which embeds `HandlerSet` +
/// `PushGossiperSet` + `Len`).
///
/// Methods take `&self` (not `&mut self`) so implementations must use
/// interior mutability — matching Go's `*BloomSet[T]`/mempool-backed sets,
/// which guard their state behind a `sync.RWMutex` and are shared via a
/// plain pointer.
pub trait Set<T: Gossipable>: Send + Sync {
    /// Adds `t` to the set.
    ///
    /// # Errors
    /// Returns an error if `t` was not added (Go `Add(v T) error`).
    fn add(&self, t: T) -> Result<()>;

    /// Returns whether `id` is a known member of the set (Go
    /// `PushGossiperSet.Has`).
    fn has(&self, id: &Id) -> bool;

    /// Iterates every item currently tracked, in implementation-defined
    /// order, stopping early if `f` returns `false` (Go `Iterate(f func(T)
    /// bool)`).
    fn iterate(&self, f: &mut dyn FnMut(&T) -> bool);

    /// Returns `(bloom_marshal_bytes, salt)` for the set's current bloom
    /// filter (Go `BloomFilter() (*bloom.Filter, ids.ID)`, wire-encoded via
    /// `bloom.Filter.Marshal`/`ids.ID` bytes so it can be shipped to peers
    /// and parsed by [`ava_utils::bloom::ReadFilter`] on either side).
    fn get_filter(&self) -> (Vec<u8>, Vec<u8>);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A minimal `Gossipable` used only to exercise object-safety below.
    struct DummyGossipable(Id);

    impl Gossipable for DummyGossipable {
        fn gossip_id(&self) -> Id {
            self.0
        }
    }

    /// A trivial in-memory `Set` impl — exists only to prove [`Set`] is
    /// object-safe (`Box<dyn Set<T>>`), per this module's design note.
    struct DummySet {
        items: Mutex<Vec<DummyGossipable>>,
    }

    impl Set<DummyGossipable> for DummySet {
        fn add(&self, t: DummyGossipable) -> Result<()> {
            self.items.lock().unwrap_or_else(|e| e.into_inner()).push(t);
            Ok(())
        }

        fn has(&self, id: &Id) -> bool {
            self.items
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|g| &g.0 == id)
        }

        fn iterate(&self, f: &mut dyn FnMut(&DummyGossipable) -> bool) {
            for item in self.items.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                if !f(item) {
                    break;
                }
            }
        }

        fn get_filter(&self) -> (Vec<u8>, Vec<u8>) {
            (Vec::new(), Vec::new())
        }
    }

    #[test]
    fn set_trait_is_object_safe_and_dispatches() {
        let set: Box<dyn Set<DummyGossipable>> = Box::new(DummySet {
            items: Mutex::new(Vec::new()),
        });
        let id = Id::from([1u8; 32]);
        set.add(DummyGossipable(id)).expect("DummySet::add");
        assert!(set.has(&id), "added id should be found via has()");

        let mut seen = 0usize;
        set.iterate(&mut |_| {
            seen += 1;
            true
        });
        assert_eq!(seen, 1, "iterate should visit the one added item");
    }
}
