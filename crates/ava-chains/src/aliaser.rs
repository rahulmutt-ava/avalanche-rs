// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The bidirectional chain [`Aliaser`] (`ids.Aliaser`, specs 07 §8.3).
//!
//! Maps human-readable aliases (`"X"`/`"C"`/`"P"`, API routes, metric
//! namespaces) to chain ids and back. An id may have many aliases; two ids may
//! not share an alias. The **primary** alias of an id is its first-registered
//! alias (Go `PrimaryAlias`). [`AliaserReader`] is the read-only `bc_lookup`
//! view threaded into the `ChainContext`.

use std::collections::HashMap;
use std::sync::RwLock;

use ava_types::id::Id;

use crate::error::{Error, Result};

/// The read-only alias lookups (`ids.AliaserReader`, the `bc_lookup` handle in
/// `ChainContext`).
pub trait AliaserReader: Send + Sync {
    /// `Lookup(alias)` — the chain id mapped to `alias`.
    ///
    /// # Errors
    /// [`Error::NotFound`] if no id is registered under `alias`.
    fn lookup(&self, alias: &str) -> Result<Id>;

    /// `PrimaryAlias(id)` — the first alias registered for `id`.
    ///
    /// # Errors
    /// [`Error::NotFound`] if `id` has no aliases.
    fn primary_alias(&self, id: Id) -> Result<String>;

    /// `Aliases(id)` — every alias of `id`, in registration order (empty if
    /// none).
    fn aliases(&self, id: Id) -> Vec<String>;
}

/// `ids.Aliaser` — bidirectional `alias ↔ chainID` with `primary_alias`.
#[derive(Debug, Default)]
pub struct Aliaser {
    inner: RwLock<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// `alias -> id`.
    dealias: HashMap<String, Id>,
    /// `id -> aliases` (in registration order; index 0 is the primary).
    aliases: HashMap<Id, Vec<String>>,
}

impl Aliaser {
    /// Builds an empty aliaser (`ids.NewAliaser`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `Alias(id, alias)` — gives `id` the alias `alias`.
    ///
    /// # Errors
    /// [`Error::AliasAlreadyInUse`] if `alias` is already mapped to any id.
    pub fn alias(&self, id: Id, alias: &str) -> Result<()> {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if inner.dealias.contains_key(alias) {
            return Err(Error::AliasAlreadyInUse {
                alias: alias.to_string(),
            });
        }
        inner.dealias.insert(alias.to_string(), id);
        inner.aliases.entry(id).or_default().push(alias.to_string());
        Ok(())
    }

    /// `RemoveAliases(id)` — drops every alias of `id`.
    pub fn remove_aliases(&self, id: Id) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(aliases) = inner.aliases.remove(&id) {
            for alias in aliases {
                inner.dealias.remove(&alias);
            }
        }
    }

    /// `PrimaryAliasOrDefault(id)` — the first alias, or the id's string form if
    /// the id has no aliases (Go default; every id is at least aliased to
    /// itself).
    #[must_use]
    pub fn primary_alias_or_default(&self, id: Id) -> String {
        self.primary_alias(id).unwrap_or_else(|_| id.to_string())
    }
}

impl AliaserReader for Aliaser {
    fn lookup(&self, alias: &str) -> Result<Id> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.dealias.get(alias).copied().ok_or(Error::NotFound)
    }

    fn primary_alias(&self, id: Id) -> Result<String> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner
            .aliases
            .get(&id)
            .and_then(|a| a.first())
            .cloned()
            .ok_or(Error::NotFound)
    }

    fn aliases(&self, id: Id) -> Vec<String> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.aliases.get(&id).cloned().unwrap_or_default()
    }
}
