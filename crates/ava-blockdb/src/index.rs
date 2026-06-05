// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Index file (`blockdb.idx`) management (Go `x/blockdb/database.go` index
//! portions).
//!
//! The index file is a 64-byte [`IndexFileHeader`] followed by fixed 16-byte
//! [`IndexEntry`] slots, one per height starting at `min_height`. This gives
//! O(1) seek by height: `offset(height) = 64 + (height - min_height) * 16`.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::error::{Error, Result};
use crate::format::{
    INDEX_FILE_VERSION, IndexEntry, IndexFileHeader, SIZE_OF_INDEX_ENTRY,
    SIZE_OF_INDEX_FILE_HEADER, UNSET_HEIGHT,
};

/// File name of the index file (Go `indexFileName`).
pub const INDEX_FILE_NAME: &str = "blockdb.idx";

/// The index file handle plus a cached copy of the header's immutable fields.
pub struct IndexFile {
    file: File,
    /// Cached header. `max_height`/`next_write_offset` here are only the values
    /// loaded at open time; the live values are tracked atomically by the store.
    pub header: IndexFileHeader,
}

impl IndexFile {
    /// Opens (creating if necessary) the index file and loads or initializes the
    /// header (Go `openAndInitializeIndex` + `loadOrInitializeHeader`).
    pub fn open(index_dir: &Path, minimum_height: u64, max_data_file_size: u64) -> Result<Self> {
        std::fs::create_dir_all(index_dir)?;
        let path = index_dir.join(INDEX_FILE_NAME);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        let size = file.metadata()?.len();
        if size == 0 {
            // Fresh index: write the initial header.
            let header = IndexFileHeader {
                version: INDEX_FILE_VERSION,
                max_data_file_size,
                min_height: minimum_height,
                max_height: UNSET_HEIGHT,
                next_write_offset: 0,
                reserved: [0u8; 24],
            };
            let bytes = header.marshal_binary();
            file.write_all_at(&bytes, 0)?;
            return Ok(Self { file, header });
        }

        let mut buf = [0u8; SIZE_OF_INDEX_FILE_HEADER as usize];
        file.read_exact_at(&mut buf, 0)?;
        let header = IndexFileHeader::unmarshal_binary(&buf)?;
        if header.version != INDEX_FILE_VERSION {
            return Err(Error::Corrupted);
        }
        Ok(Self { file, header })
    }

    /// Computes the byte offset of `height`'s index slot (Go `indexEntryOffset`).
    pub fn index_entry_offset(&self, height: u64) -> Result<u64> {
        if height < self.header.min_height {
            return Err(Error::InvalidBlockHeight);
        }
        let relative = height
            .checked_sub(self.header.min_height)
            .ok_or(Error::InvalidBlockHeight)?;
        let off = relative
            .checked_mul(SIZE_OF_INDEX_ENTRY)
            .ok_or(Error::InvalidBlockHeight)?;
        SIZE_OF_INDEX_FILE_HEADER
            .checked_add(off)
            .ok_or(Error::InvalidBlockHeight)
    }

    /// Reads the index entry for `height` (Go `readIndexEntry`).
    ///
    /// Returns [`Error::NotFound`] if the slot is past EOF or empty.
    pub fn read_index_entry(&self, height: u64) -> Result<IndexEntry> {
        let offset = self.index_entry_offset(height)?;
        let mut buf = [0u8; SIZE_OF_INDEX_ENTRY as usize];
        match self.file.read_exact_at(&mut buf, offset) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                return Err(Error::NotFound);
            }
            Err(e) => return Err(Error::Io(e)),
        }
        let entry = IndexEntry::unmarshal_binary(&buf)?;
        if entry.is_empty() {
            return Err(Error::NotFound);
        }
        Ok(entry)
    }

    /// Writes the index entry for a block (Go `writeIndexEntryAt`).
    pub fn write_index_entry(&self, height_offset: u64, data_offset: u64, size: u32) -> Result<()> {
        let entry = IndexEntry {
            offset: data_offset,
            size,
            reserved: [0u8; 4],
        };
        self.file
            .write_all_at(&entry.marshal_binary(), height_offset)?;
        Ok(())
    }

    /// Persists the header with the supplied live `next_write_offset`/`max_height`
    /// (Go `persistIndexHeaderInternal`).
    ///
    /// When `sync_to_disk` is set, the index file is fsync'd *before* the header
    /// is rewritten so the header never refers to entries that aren't durable.
    pub fn persist_header(
        &self,
        next_write_offset: u64,
        max_height: u64,
        sync_to_disk: bool,
    ) -> Result<()> {
        if sync_to_disk {
            self.file.sync_all()?;
        }
        let mut header = self.header;
        header.next_write_offset = next_write_offset;
        header.max_height = max_height;
        self.file.write_all_at(&header.marshal_binary(), 0)?;
        Ok(())
    }
}
