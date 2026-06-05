// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-archivedb` — an append-only, height-versioned key/value store.
//!
//! Tier T1 (storage). Owning spec: `specs/04-storage-and-databases.md` §5.2.
//!
//! This crate ports avalanchego's `x/archivedb`: a height-versioned KV layered
//! over any base [`ava_database::Database`]. Every write is stamped with a
//! height, and reads are taken *as of* a height — `open(height).get(key)`
//! returns the most-recent version of `key` at or below `height`.
//!
//! ## Encoding (load-bearing — byte-exact with Go so a migrated dir reads)
//!
//! - **User key →** `uvarint(len(key)) || key || BigEndian(^height)`. The
//!   bitwise-**negated** height suffix makes the base DB's ascending byte order
//!   yield **descending height**, so a forward seek from a target height lands
//!   on the newest version at/below it.
//! - **Metadata key →** `uvarint(len(key) + 1) || key`. The `+ 1` on the length
//!   prefix guarantees a metadata key can never share a prefix with any user key
//!   (whose length prefix is `len(key)`), so the two spaces never overlap.
//! - **Stored value →** `0x00 || value`; a tombstone (delete) is an empty value
//!   (see [`value`]).
//!
//! The [`HEIGHT_KEY`] metadata key (the metadata encoding of the empty key,
//! `0x01`) stores the last written height.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod value;

use ava_database::{Database, Iterator as _, helpers};

use crate::value::{new_db_value, parse_db_value};

/// Size of a big-endian-encoded `u64`, in bytes.
const U64_LEN: usize = 8;

/// Errors returned by `ava-archivedb`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The requested key has no version at or below the requested height (or was
    /// deleted at/below it). Mirrors `database.ErrNotFound`.
    #[error("not found")]
    NotFound,

    /// A stored database key could not be parsed; the underlying database is
    /// corrupted. Mirrors `archivedb.ErrParsingKeyLength`.
    #[error("failed reading key length")]
    ParsingKeyLength,

    /// A stored database key had an unexpected length; the underlying database
    /// is corrupted. Mirrors `archivedb.ErrIncorrectKeyLength`.
    #[error("incorrect key length")]
    IncorrectKeyLength,

    /// An error from the underlying base database.
    #[error(transparent)]
    Database(#[from] ava_database::Error),
}

/// A `Result` whose error is this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

// --- uvarint (Go encoding/binary PutUvarint / Uvarint, LEB128) -------------

/// Appends the LEB128 unsigned-varint encoding of `x` to `out`, returning the
/// number of bytes written (`binary.PutUvarint`).
fn put_uvarint(out: &mut Vec<u8>, mut x: u64) -> usize {
    let mut n = 0usize;
    while x >= 0x80 {
        out.push((x as u8) | 0x80);
        x >>= 7;
        n = n.saturating_add(1);
    }
    out.push(x as u8);
    n.saturating_add(1)
}

/// Decodes an LEB128 unsigned varint from the front of `buf`, returning the
/// value and the number of bytes consumed, or `None` on overflow/truncation
/// (`binary.Uvarint` returning `n <= 0`).
fn read_uvarint(buf: &[u8]) -> Option<(u64, usize)> {
    let mut x: u64 = 0;
    let mut shift: u32 = 0;
    for (i, &b) in buf.iter().enumerate() {
        if i == 10 {
            // More than 10 bytes ⇒ overflow for a u64 (binary.Uvarint).
            return None;
        }
        if b < 0x80 {
            if i == 9 && b > 1 {
                return None;
            }
            let add = u64::from(b).checked_shl(shift)?;
            return Some((x | add, i.saturating_add(1)));
        }
        let add = u64::from(b & 0x7f).checked_shl(shift)?;
        x |= add;
        shift = shift.saturating_add(7);
    }
    None
}

// --- key encoding ----------------------------------------------------------

/// Builds the database key (and its height-independent prefix) for a `(key,
/// height)` pair: `uvarint(len(key)) || key || BigEndian(^height)`
/// (`newDBKeyFromUser`).
///
/// Returns `(db_key, prefix)` where `prefix` is `db_key` without the trailing
/// 8-byte negated-height suffix.
pub fn new_db_key_from_user(key: &[u8], height: u64) -> (Vec<u8>, Vec<u8>) {
    let mut db_key = Vec::with_capacity(10usize.saturating_add(key.len()).saturating_add(U64_LEN));
    put_uvarint(&mut db_key, key.len() as u64);
    db_key.extend_from_slice(key);
    let prefix = db_key.clone();
    db_key.extend_from_slice(&(!height).to_be_bytes());
    (db_key, prefix)
}

/// Parses a user database key, returning the user key and its height
/// (`parseDBKeyFromUser`).
///
/// An error indicates a corrupted database.
pub fn parse_db_key_from_user(db_key: &[u8]) -> Result<(Vec<u8>, u64)> {
    let (key_len, offset) = read_uvarint(db_key).ok_or(Error::ParsingKeyLength)?;

    let height_index = (offset as u64).saturating_add(key_len);
    if db_key.len() as u64 != height_index.saturating_add(U64_LEN as u64) {
        return Err(Error::IncorrectKeyLength);
    }
    let height_index = height_index as usize;

    let key = db_key
        .get(offset..height_index)
        .ok_or(Error::IncorrectKeyLength)?
        .to_vec();
    let suffix: [u8; U64_LEN] = db_key
        .get(height_index..)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::IncorrectKeyLength)?;
    let height = !u64::from_be_bytes(suffix);
    Ok((key, height))
}

/// Builds the database key for a metadata `key`: `uvarint(len(key) + 1) || key`
/// (`newDBKeyFromMetadata`). The `+ 1` length prefix prevents overlap with any
/// user-key prefix.
pub fn new_db_key_from_metadata(key: &[u8]) -> Vec<u8> {
    let mut db_key = Vec::with_capacity(10usize.saturating_add(key.len()));
    put_uvarint(&mut db_key, (key.len() as u64).saturating_add(1));
    db_key.extend_from_slice(key);
    db_key
}

/// The metadata key under which the last written height is stored: the metadata
/// encoding of the empty key (`heightKey`, the byte `0x01`).
pub fn height_key() -> Vec<u8> {
    new_db_key_from_metadata(&[])
}

/// Convenience constant for the [`height_key`] bytes (`0x01`).
pub const HEIGHT_KEY: [u8; 1] = [0x01];

// --- ArchiveDb -------------------------------------------------------------

/// A height-versioned KV store layered over a base [`Database`].
///
/// See the crate docs for the encoding and read semantics.
pub struct ArchiveDb<D: Database> {
    db: D,
}

impl<D: Database> ArchiveDb<D> {
    /// Wraps `db` as an archive database.
    pub fn new(db: D) -> Self {
        Self { db }
    }

    /// Returns the last written height (the value stored under [`height_key`]).
    pub fn height(&self) -> Result<u64> {
        Ok(helpers::get_u64(&self.db, &HEIGHT_KEY)?)
    }

    /// Creates a write batch that stamps all of its operations with `height`.
    ///
    /// Committing multiple batches at the same or a lower height than the
    /// currently committed height is not an error; height consistency is the
    /// caller's responsibility (matches Go).
    pub fn new_batch(&self, height: u64) -> ArchiveBatch<'_, D> {
        ArchiveBatch {
            db: &self.db,
            height,
            ops: Vec::new(),
        }
    }

    /// Returns a [`Reader`] for the state as of `height`.
    pub fn open(&self, height: u64) -> Reader<'_, D> {
        Reader {
            db: &self.db,
            height,
        }
    }

    /// Borrows the underlying base database.
    pub fn inner(&self) -> &D {
        &self.db
    }
}

// --- Reader ----------------------------------------------------------------

/// A read view of an [`ArchiveDb`] as of a fixed height.
pub struct Reader<'a, D: Database> {
    db: &'a D,
    height: u64,
}

impl<D: Database> Reader<'_, D> {
    /// Returns whether `key` has a (non-tombstone) value at or below the read
    /// height. Mirrors `Reader.Has`.
    pub fn has(&self, key: &[u8]) -> Result<bool> {
        match self.get(key) {
            Ok(_) => Ok(true),
            Err(Error::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Returns the value of `key` as of the read height, or [`Error::NotFound`]
    /// if the key was never set, or was deleted at or below the read height.
    /// Mirrors `Reader.Get`.
    pub fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        let (value, _height, exists) = self.get_entry(key)?;
        if exists {
            Ok(value)
        } else {
            Err(Error::NotFound)
        }
    }

    /// Returns the height at which `key`'s value (as of the read height) was
    /// last set. Returns [`Error::NotFound`] if the key was never modified at or
    /// below the read height; if the last modification was a delete, returns the
    /// delete's height with the value being empty (see [`Reader::get_entry`]).
    pub fn get_height(&self, key: &[u8]) -> Result<u64> {
        let (_value, height, exists) = self.get_entry(key)?;
        if exists {
            Ok(height)
        } else {
            // The key was modified (deleted) at `height`, but has no value.
            // Mirror Get's behavior and report NotFound for a tombstone.
            Err(Error::NotFound)
        }
    }

    /// Returns `(value, height, exists)` for `key` as of the read height:
    /// the value, the height it was last modified at, and whether that last
    /// modification was an insertion (vs a tombstone). Returns
    /// [`Error::NotFound`] if `key` was never modified at or below the read
    /// height. Mirrors `Reader.GetEntry`.
    pub fn get_entry(&self, key: &[u8]) -> Result<(Vec<u8>, u64, bool)> {
        let (db_key, prefix) = new_db_key_from_user(key, self.height);

        // Seek forward from `prefix || ^height`. Because the suffix is the
        // negated height, ascending byte order is descending height, so the
        // first entry sharing `prefix` is the newest version at or below height.
        let mut it = self.db.new_iterator_with_start_and_prefix(&db_key, &prefix);

        let next = it.next();
        it.error().map_err(Error::Database)?;

        if !next {
            return Err(Error::NotFound);
        }

        let cur_key = it.key().ok_or(Error::NotFound)?;
        let (_parsed_key, height) = parse_db_key_from_user(cur_key)?;

        let cur_value = it.value().unwrap_or(&[]);
        let (value, exists) = parse_db_value(cur_value);
        Ok((value.to_vec(), height, exists))
    }
}

// --- ArchiveBatch ----------------------------------------------------------

/// A buffered op queued in an [`ArchiveBatch`].
enum Op {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

/// A write batch for an [`ArchiveDb`], stamping every op with a single height.
///
/// Operations are buffered until [`ArchiveBatch::write`], which commits them
/// atomically along with an update of the last-written height to this batch's
/// height.
pub struct ArchiveBatch<'a, D: Database> {
    db: &'a D,
    height: u64,
    ops: Vec<Op>,
}

impl<D: Database> ArchiveBatch<'_, D> {
    /// Buffers a put of `value` under `key`. Both args are copied.
    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        self.ops.push(Op::Put {
            key: key.to_vec(),
            value: value.to_vec(),
        });
    }

    /// Buffers a delete of `key` (stored as a tombstone). The arg is copied.
    pub fn delete(&mut self, key: &[u8]) {
        self.ops.push(Op::Delete { key: key.to_vec() });
    }

    /// Commits all buffered ops atomically at this batch's height, and updates
    /// the last-written height. Mirrors `batch.Write`.
    pub fn write(&mut self) -> Result<()> {
        let mut batch = self.db.new_batch();
        for op in &self.ops {
            match op {
                Op::Put { key, value } => {
                    let (db_key, _prefix) = new_db_key_from_user(key, self.height);
                    let db_value = new_db_value(value);
                    batch.put(&db_key, &db_value)?;
                }
                Op::Delete { key } => {
                    let (db_key, _prefix) = new_db_key_from_user(key, self.height);
                    // Delete ⇒ tombstone: an empty stored value.
                    batch.put(&db_key, &[])?;
                }
            }
        }
        // Update the last-written height (helpers::put_u64 == database.PutUInt64).
        batch.put(&HEIGHT_KEY, &helpers::pack_u64(self.height))?;
        batch.write()?;
        Ok(())
    }

    /// Drops buffered ops for reuse.
    pub fn reset(&mut self) {
        self.ops.clear();
    }

    /// Returns the number of buffered ops.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Returns whether the batch has no buffered ops.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}
