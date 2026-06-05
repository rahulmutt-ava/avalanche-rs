// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! A local `Maybe<T>` "something / nothing" type.
//!
//! Mirrors Go `utils/maybe.Maybe[T]`. The merkledb codec/hashing distinguish a
//! present-but-empty value (`Some(empty)`) from an absent value (`Nothing`), so
//! this is intentionally *not* `Option` at the API boundary — though it carries
//! the same shape. Defined locally here (not in `ava-types`) to keep this crate
//! self-contained (see crate docs).

/// A value that is either "something" (`Some`) or "nothing" (`Nothing`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum Maybe<T> {
    /// Absence of a value. Go `maybe.Nothing[T]()`.
    #[default]
    Nothing,
    /// A present value. Go `maybe.Some(v)`.
    Some(T),
}

impl<T> Maybe<T> {
    /// Constructs a "something" holding `value`.
    pub fn some(value: T) -> Self {
        Maybe::Some(value)
    }

    /// Constructs a "nothing".
    #[must_use]
    pub fn nothing() -> Self {
        Maybe::Nothing
    }

    /// Returns `true` iff this is a "something".
    #[must_use]
    pub fn has_value(&self) -> bool {
        matches!(self, Maybe::Some(_))
    }

    /// Returns `true` iff this is a "nothing".
    #[must_use]
    pub fn is_nothing(&self) -> bool {
        matches!(self, Maybe::Nothing)
    }

    /// Returns a reference to the contained value, if present.
    #[must_use]
    pub fn value(&self) -> Option<&T> {
        match self {
            Maybe::Some(v) => Some(v),
            Maybe::Nothing => None,
        }
    }

    /// Maps the contained value, preserving the something/nothing shape.
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Maybe<U> {
        match self {
            Maybe::Some(v) => Maybe::Some(f(v)),
            Maybe::Nothing => Maybe::Nothing,
        }
    }
}

impl<T> From<Option<T>> for Maybe<T> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(v) => Maybe::Some(v),
            None => Maybe::Nothing,
        }
    }
}
