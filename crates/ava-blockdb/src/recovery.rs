// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Torn-write recovery scan (Go `x/blockdb/database.go` `recover*`; specs/27
//! §4.1/§5.1).
//!
//! On open, the actual on-disk data size is compared with the size the index
//! header claims (`next_write_offset`). If the data files are **ahead** of the
//! index — the signature of a torn write where blocks were written but the index
//! entry / header update did not survive a crash — the scan walks forward from
//! `next_write_offset`, validates each block (header fields + checksum over the
//! decompressed bytes), rewrites the missing index entries, and recomputes
//! `max_height` — rebuilding the index identically to Go.

use crate::error::{Error, Result};
use crate::format::{
    BLOCK_ENTRY_VERSION, BlockEntryHeader, SIZE_OF_BLOCK_ENTRY_HEADER, UNSET_HEIGHT,
};
use crate::store::BlockDb;

/// Runs the recovery scan on open (Go `recover`).
pub fn recover(db: &BlockDb) -> Result<()> {
    let (files, max_index) = db.data().list()?;
    let Some(max_index) = max_index else {
        // No data files: nothing to recover.
        return Ok(());
    };

    let max_file_size = db.max_data_file_size();
    if max_file_size == u64::MAX && files.len() > 1 {
        return Err(Error::Corrupted);
    }

    // Ensure there are no gaps in the data file sequence.
    for i in 0..=max_index {
        if !files.contains_key(&i) {
            return Err(Error::Corrupted);
        }
    }

    // Compute the expected next write offset from what is actually on disk:
    // (max_index full files) + (size of the last file).
    let file_size_contribution = (max_index as u64)
        .checked_mul(max_file_size)
        .ok_or(Error::Overflow("file size contribution"))?;
    let last_file_size = db.data().file_size(max_index)?;
    let calculated_next = file_size_contribution
        .checked_add(last_file_size)
        .ok_or(Error::Overflow("last file size"))?;

    let header_next = db.load_next_write_offset();
    if calculated_next == header_next {
        // Data files match the index header; no recovery needed.
        return Ok(());
    }
    if calculated_next < header_next {
        // Index claims more data than is on disk: unrecoverable.
        return Err(Error::Corrupted);
    }

    recover_unindexed_blocks(db, header_next, calculated_next)
}

/// Scans `[start_offset, end_offset)` rebuilding index entries (Go
/// `recoverUnindexedBlocks`).
fn recover_unindexed_blocks(db: &BlockDb, start_offset: u64, end_offset: u64) -> Result<()> {
    let max_file_size = db.max_data_file_size();
    let mut scan = start_offset;
    let mut num_recovered: u64 = 0;
    let mut max_recovered: u64 = 0;

    while scan < end_offset {
        match recover_block_at_offset(db, scan, end_offset) {
            Ok(bh) => {
                num_recovered = num_recovered.saturating_add(1);
                max_recovered = max_recovered.max(bh.height);
                let block_total = (SIZE_OF_BLOCK_ENTRY_HEADER as u64)
                    .checked_add(u64::from(bh.size))
                    .ok_or(Error::Overflow("block total size"))?;
                scan = scan
                    .checked_add(block_total)
                    .ok_or(Error::Overflow("scan offset"))?;
            }
            Err(Error::Io(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // Reached the end of this file; jump to the next file's start.
                let current_file_index = scan
                    .checked_div(max_file_size)
                    .ok_or(Error::Overflow("scan / file size"))?;
                let next_file_index = current_file_index
                    .checked_add(1)
                    .ok_or(Error::Overflow("file index"))?;
                scan = next_file_index
                    .checked_mul(max_file_size)
                    .ok_or(Error::Overflow("scan offset"))?;
            }
            Err(e) => return Err(e),
        }
    }

    db.store_next_write_offset(scan);

    if num_recovered > 0 {
        let cur = db.load_max_height();
        if cur == UNSET_HEIGHT || max_recovered > cur {
            db.store_max_height(max_recovered);
        }
    }

    db.persist_index_header()?;
    Ok(())
}

/// Validates and indexes one block at `offset` (Go `recoverBlockAtOffset`).
///
/// Returns the block header on success. Returns an `UnexpectedEof` I/O error
/// when the read runs past the end of a data file (used by the caller to advance
/// to the next file).
fn recover_block_at_offset(
    db: &BlockDb,
    offset: u64,
    total_data_size: u64,
) -> Result<BlockEntryHeader> {
    let remaining = total_data_size
        .checked_sub(offset)
        .ok_or(Error::Corrupted)?;
    if remaining < u64::from(SIZE_OF_BLOCK_ENTRY_HEADER) {
        return Err(Error::Corrupted);
    }

    let mut header_buf = [0u8; SIZE_OF_BLOCK_ENTRY_HEADER as usize];
    db.data().read_at(offset, &mut header_buf)?;
    let bh = BlockEntryHeader::unmarshal_binary(&header_buf)?;

    if bh.size == 0 {
        return Err(Error::Corrupted);
    }
    if bh.version > BLOCK_ENTRY_VERSION {
        return Err(Error::Corrupted);
    }
    if bh.height < db.min_height() || bh.height == UNSET_HEIGHT {
        return Err(Error::Corrupted);
    }

    let block_end = offset
        .checked_add(u64::from(SIZE_OF_BLOCK_ENTRY_HEADER))
        .and_then(|v| v.checked_add(u64::from(bh.size)))
        .ok_or(Error::Overflow("block end offset"))?;
    if block_end > total_data_size {
        return Err(Error::Corrupted);
    }

    let data_offset = offset
        .checked_add(u64::from(SIZE_OF_BLOCK_ENTRY_HEADER))
        .ok_or(Error::Overflow("block data offset"))?;
    let mut block_data = vec![0u8; bh.size as usize];
    db.data().read_at(data_offset, &mut block_data)?;

    let decompressed = db.decompress(&block_data)?;
    let calculated = crate::format::calculate_checksum(&decompressed);
    if calculated != bh.checksum {
        return Err(Error::Corrupted);
    }

    let index_offset = db.index().index_entry_offset(bh.height)?;
    db.index()
        .write_index_entry(index_offset, offset, bh.size)?;
    Ok(bh)
}
