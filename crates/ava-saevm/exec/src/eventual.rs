// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`Eventual<T>`] — a set-once, awaitable cell (Go `eventual.Value[*Receipt]`).
//!
//! Backs the per-tx receipt buffer: a caller can `wait()` for a transaction's
//! receipt before — or after — the block that produces it executes, and the
//! executor `set`s it exactly once when the receipt is published (specs/11 §6.1
//! step 6, §11 mapping `eventual.Value[*Receipt]` → "a `OnceCell`/`watch`-backed
//! `Eventual<T>`").
//!
//! Implemented over [`tokio::sync::watch`]: the channel holds `Option<T>`,
//! starts `None`, and the **first** [`set`](Eventual::set) wins (stores
//! `Some(v)`); subsequent sets are rejected. [`wait`](Eventual::wait) awaits the
//! watch until the value is `Some`. A `watch` receiver that is created after the
//! value is set still observes it (the sender retains the current value), so
//! publish-before-wait resolves immediately.

use tokio::sync::watch;

/// A set-once, awaitable cell of a cloneable value (Go `eventual.Value[T]`).
///
/// Cloning an [`Eventual`] yields another handle onto the **same** cell, so a
/// producer and any number of waiters share one resolution.
#[derive(Clone, Debug)]
pub struct Eventual<T: Clone> {
    tx: watch::Sender<Option<T>>,
}

impl<T: Clone> Default for Eventual<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> Eventual<T> {
    /// Constructs an unresolved cell.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(None);
        Self { tx }
    }

    /// Resolves the cell to `value`, first-write-wins.
    ///
    /// Returns `true` if this call set the value, `false` if it was already set
    /// (idempotent / set-once — a later set never overwrites the first). Waking
    /// the waiters happens *after* the value is stored, so any woken `wait()`
    /// reads the resolved value.
    pub fn set(&self, value: T) -> bool {
        let mut stored = false;
        self.tx.send_if_modified(|slot| {
            if slot.is_none() {
                *slot = Some(value);
                stored = true;
                true
            } else {
                false
            }
        });
        stored
    }

    /// The current value, if resolved.
    #[must_use]
    pub fn get(&self) -> Option<T> {
        self.tx.borrow().clone()
    }

    /// Awaits until the cell is resolved, then returns the value.
    ///
    /// Resolves immediately if the value was set before this call (the watch
    /// sender retains the current value for late subscribers). Loops on spurious
    /// wakeups; the value is set-once so the loop terminates as soon as it reads
    /// `Some`.
    pub async fn wait(&self) -> T {
        let mut rx = self.tx.subscribe();
        // Fast path: already resolved.
        if let Some(v) = rx.borrow_and_update().clone() {
            return v;
        }
        loop {
            // `changed()` errors only if every sender dropped; this handle holds
            // a sender, so it cannot. Treat the (impossible) error as a benign
            // re-check rather than unwrapping.
            if rx.changed().await.is_err() {
                if let Some(v) = self.get() {
                    return v;
                }
                // No sender and no value: yield and re-loop (cannot occur while
                // `self` is alive, but stays panic-free).
                tokio::task::yield_now().await;
                continue;
            }
            if let Some(v) = rx.borrow_and_update().clone() {
                return v;
            }
        }
    }
}
