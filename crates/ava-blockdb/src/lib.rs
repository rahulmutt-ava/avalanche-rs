// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-blockdb` — an append-optimized, height-indexed block store.
//!
//! Byte-exact Rust port of avalanchego `x/blockdb`. See specs/04 §5.1 and
//! specs/27 §4.1/§5.1. The store keeps:
//!
//! - one **index file** (`blockdb.idx`): a 64-byte [`format::IndexFileHeader`]
//!   followed by fixed 16-byte [`format::IndexEntry`] slots (O(1) seek by
//!   height); and
//! - multiple **data files** (`blockdb_<n>.dat`): each block is a 22-byte
//!   [`format::BlockEntryHeader`] followed by the (zstd-compressed) block bytes,
//!   split across files at `max_data_file_size`.
//!
//! On open the store runs a [`recovery`] scan: if the data files are larger than
//! the index claims (a torn write), it scans forward from `next_write_offset`,
//! validates each block header + checksum, and rebuilds the missing index
//! entries — identically to Go.

#![forbid(unsafe_code)]

mod config;
mod data;
pub mod error;
pub mod format;
mod index;
mod recovery;
mod store;

pub use config::DatabaseConfig;
pub use error::{Error, Result};
pub use store::BlockDb;

/// Type alias for block heights (Go `BlockHeight`).
pub type BlockHeight = u64;

/// Type alias for block data (Go `BlockData`).
pub type BlockData = Vec<u8>;
