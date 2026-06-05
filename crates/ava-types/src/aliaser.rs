// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Bidirectional id<->alias mapping.
//!
//! Mirrors Go `ids.Aliaser` (`ids/aliases.go`): a bidirectional map
//! `alias -> id` and `id -> Vec<alias>`, guarded by a `parking_lot::RwLock`.
//! An alias maps to exactly one id; one id may have many aliases (the first is
//! "primary"). Owning spec: `specs/03-core-primitives.md` §1.3.

use std::collections::HashMap;

use parking_lot::RwLock;

use crate::error::{Error, Result};
use crate::id::Id;

#[derive(Default)]
struct Inner {
    /// alias -> id.
    dealias: HashMap<String, Id>,
    /// id -> ordered aliases (first is primary).
    aliases: HashMap<Id, Vec<String>>,
}

/// Bidirectional id<->alias map. Thread-safe. Mirrors Go `ids.Aliaser`.
#[derive(Default)]
pub struct Aliaser {
    inner: RwLock<Inner>,
}

impl Aliaser {
    /// Creates an empty aliaser. Mirrors `ids.NewAliaser`.
    #[must_use]
    pub fn new() -> Aliaser {
        Aliaser::default()
    }

    /// Returns the id associated with `alias`.
    ///
    /// # Errors
    /// Returns [`Error::NoIdWithAlias`] if `alias` is not mapped.
    pub fn lookup(&self, alias: &str) -> Result<Id> {
        let inner = self.inner.read();
        inner
            .dealias
            .get(alias)
            .copied()
            .ok_or_else(|| Error::NoIdWithAlias(alias.to_string()))
    }

    /// Returns the first ("primary") alias of `id`.
    ///
    /// # Errors
    /// Returns [`Error::NoIdWithAlias`] if `id` has no aliases (mirrors Go's
    /// `errNoAliasForID`, collapsed onto the same variant for this crate).
    pub fn primary_alias(&self, id: Id) -> Result<String> {
        let inner = self.inner.read();
        match inner.aliases.get(&id).and_then(|v| v.first()) {
            Some(alias) => Ok(alias.clone()),
            None => Err(Error::NoIdWithAlias(id.hex())),
        }
    }

    /// Returns the first alias of `id`, or the id string form if none exists.
    /// Mirrors Go `PrimaryAliasOrDefault`.
    #[must_use]
    pub fn primary_alias_or_default(&self, id: Id) -> String {
        // TODO(M0.6): use CB58 Display once available.
        self.primary_alias(id).unwrap_or_else(|_| id.hex())
    }

    /// Returns all aliases of `id`, in insertion order (empty if none).
    /// Mirrors Go `Aliases`.
    #[must_use]
    pub fn aliases(&self, id: Id) -> Vec<String> {
        let inner = self.inner.read();
        inner.aliases.get(&id).cloned().unwrap_or_default()
    }

    /// Gives `id` the alias `alias`.
    ///
    /// # Errors
    /// Returns [`Error::AliasAlreadyMapped`] if `alias` is already mapped to
    /// some id.
    pub fn alias(&self, id: Id, alias: &str) -> Result<()> {
        let mut inner = self.inner.write();
        if inner.dealias.contains_key(alias) {
            return Err(Error::AliasAlreadyMapped(alias.to_string()));
        }
        inner.dealias.insert(alias.to_string(), id);
        inner.aliases.entry(id).or_default().push(alias.to_string());
        Ok(())
    }

    /// Removes all aliases of `id`. Mirrors Go `RemoveAliases`.
    pub fn remove_aliases(&self, id: Id) {
        let mut inner = self.inner.write();
        if let Some(aliases) = inner.aliases.remove(&id) {
            for alias in aliases {
                inner.dealias.remove(&alias);
            }
        }
    }

    /// Returns, per id, its aliases with the redundant self-alias removed
    /// (`alias == id.to_string()`). Mirrors Go `GetRelevantAliases`.
    ///
    /// # Errors
    /// Infallible in this implementation, but returns `Result` to mirror the
    /// Go signature (which can surface a lookup error).
    pub fn get_relevant_aliases(&self, ids: &[Id]) -> Result<HashMap<Id, Vec<String>>> {
        let inner = self.inner.read();
        let mut result = HashMap::with_capacity(ids.len());
        for &id in ids {
            // TODO(M0.6): use CB58 Display once available for the self-alias key.
            let self_alias = id.hex();
            let relevant = inner
                .aliases
                .get(&id)
                .map(|aliases| {
                    aliases
                        .iter()
                        .filter(|alias| **alias != self_alias)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            result.insert(id, relevant);
        }
        Ok(result)
    }
}
