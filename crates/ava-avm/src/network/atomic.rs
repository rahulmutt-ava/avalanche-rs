// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Atomic app-handler switch for the X-Chain gossip layer (specs 09 §8;
//! `vms/avm/network/atomic.go`).
//!
//! ## Design
//!
//! The `ava_vm::AppHandler` trait's methods take `&mut self`, which an
//! `Arc<dyn AppHandler>` (shared) cannot call. Rather than fight this, we
//! define a **local** [`AppGossipHandler`] trait whose `handle_app_gossip`
//! takes `&self` — the gossip handler is effectively stateless (the mempool
//! it mutates is owned+locked by the VM, not by the app-handler) — and build
//! the atomic switch over that local trait.
//!
//! The [`AtomicAppHandler`] holds an `ArcSwap<Arc<dyn AppGossipHandler>>`,
//! enabling lock-free reads on the hot path and non-blocking swaps when the
//! VM transitions (e.g., bootstrapping completes). The actual wiring of the
//! VM's `AppHandler::app_gossip` to call this switch is **M5.19** — note
//! clearly deferred there.
//!
//! ## Deferred
//!
//! * Wiring `AtomicAppHandler` into the VM's `AppHandler::app_gossip` impl
//!   (construction of the real live handler + initial install) is M5.19.

use std::sync::Arc;

use arc_swap::ArcSwap;
use ava_types::node_id::NodeId;

/// `AppGossipHandler` — the `&self` gossip-message handler interface.
///
/// Unlike `ava_vm::AppHandler` (which takes `&mut self`), this trait takes
/// `&self` so it can be called through `Arc<dyn AppGossipHandler>`. The real
/// gossip handler is stateless — it mutates the `Mempool` via a lock owned by
/// the VM, not by the handler itself.
///
/// Mirrors `vms/avm/network/atomic.go` `AppGossipHandler`.
pub trait AppGossipHandler: Send + Sync {
    /// Handle an inbound gossip message from `node`.
    fn handle_app_gossip(&self, node: NodeId, msg: &[u8]);
}

/// An atomic (lock-free) switch over an [`AppGossipHandler`], backed by
/// [`ArcSwap`] (`vms/avm/network/atomic.go`).
///
/// The switch enables the VM to replace the live gossip handler without
/// blocking the hot gossip-message path — `load` is a single atomic read,
/// `swap` is a single atomic write. The hot path calls [`load`](Self::load)
/// then dispatches; the VM's initialization or post-bootstrap transition calls
/// [`swap`](Self::swap).
///
/// ## Deferred (M5.19)
///
/// Wiring `AtomicAppHandler` into the VM's `AppHandler::app_gossip` method
/// (constructing the real `TxGossipHandler`-backed live handler and installing
/// it here) is deferred to M5.19, which assembles the full VM struct.
pub struct AtomicAppHandler {
    /// The currently-live gossip handler, swappable without blocking readers.
    inner: ArcSwap<Arc<dyn AppGossipHandler>>,
}

impl AtomicAppHandler {
    /// Builds a new switch with `handler` as the initial live handler.
    #[must_use]
    pub fn new(handler: Arc<dyn AppGossipHandler>) -> Self {
        Self {
            inner: ArcSwap::from_pointee(handler),
        }
    }

    /// Atomically replaces the current handler with `new_handler`.
    ///
    /// Concurrent `handle_app_gossip` calls that already loaded the previous
    /// handler finish against the old handler; subsequent loads see
    /// `new_handler`.
    pub fn swap(&self, new_handler: Arc<dyn AppGossipHandler>) {
        self.inner.store(Arc::new(new_handler));
    }

    /// Loads the current handler (a single atomic read — no blocking).
    #[must_use]
    pub fn load(&self) -> Arc<dyn AppGossipHandler> {
        // ArcSwap::load returns an `arc_swap::Guard` (a smart pointer);
        // clone the inner `Arc<Arc<…>>` and deref once to get `Arc<dyn …>`.
        Arc::clone(&*self.inner.load())
    }

    /// Dispatches an inbound gossip message to the current live handler.
    pub fn handle_app_gossip(&self, node: NodeId, msg: &[u8]) {
        self.load().handle_app_gossip(node, msg);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Counter(Arc<AtomicUsize>);

    impl AppGossipHandler for Counter {
        fn handle_app_gossip(&self, _node: NodeId, _msg: &[u8]) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn swap_routes_to_current_handler() {
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));

        let ha: Arc<dyn AppGossipHandler> = Arc::new(Counter(Arc::clone(&a)));
        let hb: Arc<dyn AppGossipHandler> = Arc::new(Counter(Arc::clone(&b)));

        let switch = AtomicAppHandler::new(Arc::clone(&ha));

        switch.handle_app_gossip(NodeId::default(), &[]);
        assert_eq!(a.load(Ordering::SeqCst), 1);
        assert_eq!(b.load(Ordering::SeqCst), 0);

        switch.swap(Arc::clone(&hb));
        switch.handle_app_gossip(NodeId::default(), &[]);
        assert_eq!(a.load(Ordering::SeqCst), 1); // unchanged
        assert_eq!(b.load(Ordering::SeqCst), 1);

        switch.swap(ha);
        switch.handle_app_gossip(NodeId::default(), &[]);
        assert_eq!(a.load(Ordering::SeqCst), 2);
        assert_eq!(b.load(Ordering::SeqCst), 1);
    }
}
