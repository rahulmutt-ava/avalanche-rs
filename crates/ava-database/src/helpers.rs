// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Byte-exact free-function helpers, mirroring `database/helpers.go` (04 §1.4).
//!
//! These layouts are part of the on-disk compatibility surface (some persisted
//! records embed them — notably timestamps), so they reproduce the Go encoding
//! bit-for-bit. All integers are **big-endian** (`database.PackUInt64`); `bool`
//! is a single `0x00`/`0x01` byte.

use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::traits::{
    Database, Iteratee, Iterator, KeyValueDeleter, KeyValueReader, KeyValueWriter,
};

/// Size of a packed `u64`, in bytes (`database.Uint64Size`).
pub const U64_SIZE: usize = 8;
/// Size of a packed `u32`, in bytes.
pub const U32_SIZE: usize = 4;
/// Size of a packed `bool`, in bytes (`database.BoolSize`).
pub const BOOL_SIZE: usize = 1;
/// The byte for `false` (`database.BoolFalse`).
pub const BOOL_FALSE: u8 = 0x00;
/// The byte for `true` (`database.BoolTrue`).
pub const BOOL_TRUE: u8 = 0x01;

/// Estimated per-pair overhead used by [`size`] (`database.kvPairOverhead`).
pub const KV_PAIR_OVERHEAD: usize = 8;

// --- ID -------------------------------------------------------------------

/// Stores `val` (32 bytes) under `key` (`database.PutID`).
pub fn put_id<W: KeyValueWriter + ?Sized>(db: &W, key: &[u8], val: &Id) -> Result<()> {
    db.put(key, val.as_bytes())
}

/// Reads a 32-byte [`Id`] from `key` (`database.GetID`).
pub fn get_id<R: KeyValueReader + ?Sized>(db: &R, key: &[u8]) -> Result<Id> {
    let b = db.get(key)?;
    Id::from_slice(&b).map_err(|e| Error::Other(anyhow::anyhow!("{e}")))
}

// --- u64 ------------------------------------------------------------------

/// Big-endian-packs `val` into 8 bytes (`database.PackUInt64`).
pub fn pack_u64(val: u64) -> [u8; U64_SIZE] {
    val.to_be_bytes()
}

/// Parses a big-endian `u64` from exactly 8 bytes (`database.ParseUInt64`).
pub fn parse_u64(b: &[u8]) -> Result<u64> {
    let arr: [u8; U64_SIZE] = b
        .try_into()
        .map_err(|_| Error::Other(anyhow::anyhow!("value has unexpected size")))?;
    Ok(u64::from_be_bytes(arr))
}

/// Stores `val` big-endian under `key` (`database.PutUInt64`).
pub fn put_u64<W: KeyValueWriter + ?Sized>(db: &W, key: &[u8], val: u64) -> Result<()> {
    db.put(key, &pack_u64(val))
}

/// Reads a big-endian `u64` from `key` (`database.GetUInt64`).
pub fn get_u64<R: KeyValueReader + ?Sized>(db: &R, key: &[u8]) -> Result<u64> {
    let b = db.get(key)?;
    parse_u64(&b)
}

// --- u32 ------------------------------------------------------------------

/// Big-endian-packs `val` into 4 bytes (`database.PackUInt32`).
pub fn pack_u32(val: u32) -> [u8; U32_SIZE] {
    val.to_be_bytes()
}

/// Parses a big-endian `u32` from exactly 4 bytes (`database.ParseUInt32`).
pub fn parse_u32(b: &[u8]) -> Result<u32> {
    let arr: [u8; U32_SIZE] = b
        .try_into()
        .map_err(|_| Error::Other(anyhow::anyhow!("value has unexpected size")))?;
    Ok(u32::from_be_bytes(arr))
}

/// Stores `val` big-endian under `key` (`database.PutUInt32`).
pub fn put_u32<W: KeyValueWriter + ?Sized>(db: &W, key: &[u8], val: u32) -> Result<()> {
    db.put(key, &pack_u32(val))
}

/// Reads a big-endian `u32` from `key` (`database.GetUInt32`).
pub fn get_u32<R: KeyValueReader + ?Sized>(db: &R, key: &[u8]) -> Result<u32> {
    let b = db.get(key)?;
    parse_u32(&b)
}

// --- bool -----------------------------------------------------------------

/// Encodes a `bool` as a single byte (`0x01`/`0x00`).
pub fn pack_bool(b: bool) -> [u8; BOOL_SIZE] {
    if b { [BOOL_TRUE] } else { [BOOL_FALSE] }
}

/// Decodes a single-byte `bool` (`database.GetBool` value check).
pub fn parse_bool(b: &[u8]) -> Result<bool> {
    match b {
        [BOOL_FALSE] => Ok(false),
        [BOOL_TRUE] => Ok(true),
        [other] => Err(Error::Other(anyhow::anyhow!(
            "should be {BOOL_FALSE} or {BOOL_TRUE} but is {other}"
        ))),
        _ => Err(Error::Other(anyhow::anyhow!(
            "length should be {BOOL_SIZE} but is {}",
            b.len()
        ))),
    }
}

/// Stores `b` as a single byte under `key` (`database.PutBool`).
pub fn put_bool<W: KeyValueWriter + ?Sized>(db: &W, key: &[u8], b: bool) -> Result<()> {
    db.put(key, &pack_bool(b))
}

/// Reads a single-byte `bool` from `key` (`database.GetBool`).
pub fn get_bool<R: KeyValueReader + ?Sized>(db: &R, key: &[u8]) -> Result<bool> {
    let b = db.get(key)?;
    parse_bool(&b)
}

// --- timestamp ------------------------------------------------------------

/// Seconds between absolute zero (Jan 1, year 1, the Go `time` internal epoch)
/// and the Unix epoch (Jan 1, 1970). `time.Time.MarshalBinary` stores seconds
/// relative to absolute zero.
const UNIX_TO_INTERNAL: i64 = (1969 * 365 + 1969 / 4 - 1969 / 100 + 1969 / 400) * 86400;

/// The version byte of `time.Time.MarshalBinary`'s v1 format.
const TIME_BINARY_VERSION_V1: u8 = 1;

/// A timestamp that round-trips Go's `time.Time.MarshalBinary` byte-for-byte.
///
/// Go's wire format (15 bytes, version 1):
/// `[version:u8][seconds_since_abs_zero:i64 BE][nanos:i32 BE][offset_min:i16 BE]`
/// where an offset of `-1` (`0xffff`) denotes UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamp {
    /// Seconds since the Unix epoch (may be negative).
    pub unix_secs: i64,
    /// Sub-second nanoseconds (`0..1_000_000_000`).
    pub nanos: u32,
    /// Zone offset in minutes; `-1` denotes UTC (matches Go's sentinel).
    pub offset_minutes: i16,
}

impl Timestamp {
    /// A UTC timestamp at `unix_secs`.`nanos` (offset sentinel `-1`).
    pub fn from_unix_utc(unix_secs: i64, nanos: u32) -> Self {
        Self {
            unix_secs,
            nanos,
            offset_minutes: -1,
        }
    }

    /// Encodes to the 15-byte Go `time.Time.MarshalBinary` v1 format.
    pub fn marshal_binary(&self) -> Vec<u8> {
        let abs_secs = self.unix_secs.wrapping_add(UNIX_TO_INTERNAL);
        let mut out = Vec::with_capacity(15);
        out.push(TIME_BINARY_VERSION_V1);
        out.extend_from_slice(&abs_secs.to_be_bytes());
        out.extend_from_slice(&(self.nanos as i32).to_be_bytes());
        out.extend_from_slice(&self.offset_minutes.to_be_bytes());
        out
    }

    /// Decodes from Go's `time.Time.MarshalBinary` v1 format.
    pub fn unmarshal_binary(b: &[u8]) -> Result<Self> {
        let arr: [u8; 15] = b
            .try_into()
            .map_err(|_| Error::Other(anyhow::anyhow!("Time.UnmarshalBinary: invalid length")))?;
        let [
            version,
            s0,
            s1,
            s2,
            s3,
            s4,
            s5,
            s6,
            s7,
            n0,
            n1,
            n2,
            n3,
            o0,
            o1,
        ] = arr;
        if version != TIME_BINARY_VERSION_V1 {
            return Err(Error::Other(anyhow::anyhow!(
                "Time.UnmarshalBinary: unsupported version"
            )));
        }
        let abs_secs = i64::from_be_bytes([s0, s1, s2, s3, s4, s5, s6, s7]);
        let nanos = i32::from_be_bytes([n0, n1, n2, n3]);
        let offset_minutes = i16::from_be_bytes([o0, o1]);
        Ok(Self {
            unix_secs: abs_secs.wrapping_sub(UNIX_TO_INTERNAL),
            nanos: nanos as u32,
            offset_minutes,
        })
    }
}

/// Stores `val` using Go's `time.Time.MarshalBinary` format (`database.PutTimestamp`).
pub fn put_timestamp<W: KeyValueWriter + ?Sized>(
    db: &W,
    key: &[u8],
    val: &Timestamp,
) -> Result<()> {
    db.put(key, &val.marshal_binary())
}

/// Reads a [`Timestamp`] from `key` (`database.GetTimestamp`).
pub fn get_timestamp<R: KeyValueReader + ?Sized>(db: &R, key: &[u8]) -> Result<Timestamp> {
    let b = db.get(key)?;
    Timestamp::unmarshal_binary(&b)
}

// --- with_default / count / size ------------------------------------------

/// Returns the result of `get(db, key)`, substituting `def` on
/// [`Error::NotFound`] (`database.WithDefault`).
pub fn with_default<R, V>(
    get: impl Fn(&R, &[u8]) -> Result<V>,
    db: &R,
    key: &[u8],
    def: V,
) -> Result<V>
where
    R: KeyValueReader + ?Sized,
{
    match get(db, key) {
        Err(Error::NotFound) => Ok(def),
        other => other,
    }
}

/// Counts all entries via a full iteration (`database.Count`).
pub fn count<I: Iteratee + ?Sized>(db: &I) -> Result<usize> {
    let mut it = db.new_iterator();
    let mut n = 0usize;
    while it.next() {
        n = n.saturating_add(1);
    }
    it.error()?;
    Ok(n)
}

/// Estimates total stored bytes (`len(key)+len(value)+KV_PAIR_OVERHEAD` per
/// entry) via a full iteration (`database.Size`).
pub fn size<I: Iteratee + ?Sized>(db: &I) -> Result<usize> {
    let mut it = db.new_iterator();
    let mut total = 0usize;
    while it.next() {
        let k = it.key().map_or(0, <[u8]>::len);
        let v = it.value().map_or(0, <[u8]>::len);
        total = total
            .saturating_add(k)
            .saturating_add(v)
            .saturating_add(KV_PAIR_OVERHEAD);
    }
    it.error()?;
    Ok(total)
}

// --- clear ----------------------------------------------------------------

/// Deletes every entry of `reader_db` from `deleter_db` (`database.AtomicClear`).
pub fn atomic_clear<R, D>(reader_db: &R, deleter_db: &D) -> Result<()>
where
    R: Iteratee + ?Sized,
    D: KeyValueDeleter + ?Sized,
{
    atomic_clear_prefix(reader_db, deleter_db, &[])
}

/// Deletes every entry of `reader_db` with `prefix` from `deleter_db`
/// (`database.AtomicClearPrefix`).
pub fn atomic_clear_prefix<R, D>(reader_db: &R, deleter_db: &D, prefix: &[u8]) -> Result<()>
where
    R: Iteratee + ?Sized,
    D: KeyValueDeleter + ?Sized,
{
    let mut it = reader_db.new_iterator_with_prefix(prefix);
    while it.next() {
        if let Some(key) = it.key() {
            deleter_db.delete(key)?;
        }
    }
    it.error()
}

/// Removes all key/value pairs from `db`, batching writes at `write_size`
/// bytes (`database.Clear`).
pub fn clear<D: Database + ?Sized>(db: &D, write_size: usize) -> Result<()> {
    clear_prefix(db, &[], write_size)
}

/// Removes all keys with `prefix` from `db`, batching writes at `write_size`
/// bytes and re-seeking the iterator after each flush to release references to
/// now-deleted keys (`database.ClearPrefix`).
pub fn clear_prefix<D: Database + ?Sized>(db: &D, prefix: &[u8], write_size: usize) -> Result<()> {
    let mut b = db.new_batch();
    let mut it = db.new_iterator_with_prefix(prefix);

    while it.next() {
        let Some(key) = it.key() else { continue };
        b.delete(key)?;

        // Avoid memory pressure by periodically flushing.
        if b.size() < write_size {
            continue;
        }
        b.write()?;
        b.reset();

        // Re-seek the iterator to release references to deleted keys.
        it.error()?;
        it.release();
        it = db.new_iterator_with_prefix(prefix);
    }

    b.write()?;
    it.error()
}

#[cfg(test)]
mod tests {
    use ava_types::id::ID_LEN;

    use super::*;

    #[test]
    fn helpers_byte_exact() {
        // put_u64 big-endian (Go PackUInt64).
        assert_eq!(pack_u64(0x0102_0304_0506_0708), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            parse_u64(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap(),
            0x0102_0304_0506_0708
        );

        // put_u32 big-endian.
        assert_eq!(pack_u32(0x0102_0304), [1, 2, 3, 4]);
        assert_eq!(parse_u32(&[1, 2, 3, 4]).unwrap(), 0x0102_0304);

        // put_bool / get_bool.
        assert_eq!(pack_bool(true), [0x01]);
        assert_eq!(pack_bool(false), [0x00]);
        assert!(parse_bool(&[0x01]).unwrap());
        assert!(!parse_bool(&[0x00]).unwrap());
        assert!(parse_bool(&[0x02]).is_err());
        assert!(parse_bool(&[]).is_err());
        assert!(parse_bool(&[0, 0]).is_err());

        // wrong-size integer parses reject.
        assert!(parse_u64(&[1, 2, 3]).is_err());
        assert!(parse_u32(&[1, 2, 3]).is_err());
    }

    #[test]
    fn timestamp_byte_exact_go_marshalbinary() {
        // Golden bytes extracted from Go's time.Time.MarshalBinary (UTC, v1).
        // unix epoch (1970-01-01T00:00:00Z) -> "010000000e7791f70000000000ffff".
        let t = Timestamp::from_unix_utc(0, 0);
        assert_eq!(
            hex::encode(t.marshal_binary()),
            "010000000e7791f70000000000ffff"
        );

        // 2020-01-02T03:04:05Z (unix 1577934245).
        let t = Timestamp::from_unix_utc(1_577_934_245, 0);
        assert_eq!(
            hex::encode(t.marshal_binary()),
            "010000000ed59f54a500000000ffff"
        );

        // 2021-06-07T08:09:10.123456789Z (unix 1623053350, nanos 123456789).
        let t = Timestamp::from_unix_utc(1_623_053_350, 123_456_789);
        assert_eq!(
            hex::encode(t.marshal_binary()),
            "010000000ed84fcb26075bcd15ffff"
        );

        // Round-trip.
        for bytes_hex in [
            "010000000e7791f70000000000ffff",
            "010000000ed84fcb26075bcd15ffff",
        ] {
            let b = hex::decode(bytes_hex).unwrap();
            let parsed = Timestamp::unmarshal_binary(&b).unwrap();
            assert_eq!(hex::encode(parsed.marshal_binary()), bytes_hex);
        }
    }

    #[test]
    fn id_roundtrip_helpers() {
        let id = Id::from_slice(&[7u8; ID_LEN]).unwrap();
        let bytes = id.as_bytes();
        assert_eq!(bytes.len(), ID_LEN);
    }
}
