// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The [`BlockDb`] block store (Go `x/blockdb/database.go` `Database`).

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use lru::LruCache;
use parking_lot::{Mutex, RwLock};

use crate::config::DatabaseConfig;
use crate::data::{DataFiles, allocate_block_space};
use crate::error::{Error, Result};
use crate::format::{
    BLOCK_ENTRY_VERSION, BlockEntryHeader, SIZE_OF_BLOCK_ENTRY_HEADER, UNSET_HEIGHT,
    calculate_checksum,
};
use crate::index::IndexFile;
use crate::recovery::recover;
use crate::{BlockData, BlockHeight};

/// Append-optimized, height-indexed block store.
///
/// Reads use positioned I/O on per-file handles so they never block writes; the
/// write offset is tracked in an atomic so the hot write path is `RwLock`-free.
pub struct BlockDb {
    config: DatabaseConfig,
    index: IndexFile,
    data: DataFiles,
    block_cache: Option<Mutex<LruCache<BlockHeight, BlockData>>>,

    /// Highest written height, or [`UNSET_HEIGHT`].
    max_block_height: AtomicU64,
    /// Next global offset to write new data.
    next_write_offset: AtomicU64,
    /// Guards the index header rewrite (mirrors Go `headerWriteOccupied`).
    header_write_lock: RwLock<()>,
    closed: AtomicBool,
}

impl BlockDb {
    /// Opens (creating if necessary) a block store at the configured paths,
    /// running the recovery scan on open (Go `New`).
    ///
    /// # Errors
    /// Returns an error if the config is invalid, the files cannot be opened, or
    /// recovery detects unrecoverable corruption.
    pub fn open(config: DatabaseConfig) -> Result<Self> {
        config.validate()?;

        let index = IndexFile::open(
            &config.index_dir,
            config.minimum_height,
            config.max_data_file_size,
        )?;
        // The header on disk carries the live values when reopening.
        let next_write_offset = AtomicU64::new(index.header.next_write_offset);
        let max_block_height = AtomicU64::new(index.header.max_height);

        let data = DataFiles::open(
            &config.data_dir,
            // The header's max_data_file_size is authoritative once initialized.
            index.header.max_data_file_size,
            config.max_data_files,
            config.sync_to_disk,
        )?;

        let block_cache = if config.block_cache_size > 0 {
            NonZeroUsize::new(usize::from(config.block_cache_size))
                .map(|cap| Mutex::new(LruCache::new(cap)))
        } else {
            None
        };

        let db = Self {
            config,
            index,
            data,
            block_cache,
            max_block_height,
            next_write_offset,
            header_write_lock: RwLock::new(()),
            closed: AtomicBool::new(false),
        };

        recover(&db)?;

        Ok(db)
    }

    /// The maximum data file size in effect (from the index header).
    pub(crate) fn max_data_file_size(&self) -> u64 {
        self.index.header.max_data_file_size
    }

    pub(crate) fn min_height(&self) -> u64 {
        self.index.header.min_height
    }

    pub(crate) fn index(&self) -> &IndexFile {
        &self.index
    }

    pub(crate) fn data(&self) -> &DataFiles {
        &self.data
    }

    pub(crate) fn load_next_write_offset(&self) -> u64 {
        self.next_write_offset.load(Ordering::Acquire)
    }

    pub(crate) fn store_next_write_offset(&self, v: u64) {
        self.next_write_offset.store(v, Ordering::Release);
    }

    pub(crate) fn load_max_height(&self) -> u64 {
        self.max_block_height.load(Ordering::Acquire)
    }

    pub(crate) fn store_max_height(&self, v: u64) {
        self.max_block_height.store(v, Ordering::Release);
    }

    /// Decompresses raw stored block bytes.
    pub(crate) fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        zstd::stream::decode_all(data).map_err(|e| Error::Compression(e.to_string()))
    }

    fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        // zstd level 1 == BestSpeed (Go uses zstd.BestSpeed).
        zstd::stream::encode_all(data, 1).map_err(|e| Error::Compression(e.to_string()))
    }

    /// Persists the index header with the current live offset/max-height
    /// (Go `persistIndexHeader`). Single-flighted via the header write lock.
    pub(crate) fn persist_index_header(&self) -> Result<()> {
        let Some(_guard) = self.header_write_lock.try_write() else {
            // A concurrent persist is in progress; skip (Go behavior).
            return Ok(());
        };
        self.index.persist_header(
            self.load_next_write_offset(),
            self.load_max_height(),
            self.config.sync_to_disk,
        )
    }

    /// Inserts a block at `height` (Go `Put`).
    ///
    /// # Errors
    /// Returns [`Error::Closed`] if closed, [`Error::BlockTooLarge`] if the block
    /// is too large, or [`Error::InvalidBlockHeight`] for an out-of-range height.
    pub fn put(&self, height: BlockHeight, block: &[u8]) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        if u64::try_from(block.len())
            .map(|n| n > u64::from(u32::MAX))
            .unwrap_or(true)
        {
            return Err(Error::BlockTooLarge);
        }

        let index_offset = self.index.index_entry_offset(height)?;

        let compressed = self.compress(block)?;
        let compressed_len = u32::try_from(compressed.len()).map_err(|_| Error::BlockTooLarge)?;
        let total_size = SIZE_OF_BLOCK_ENTRY_HEADER
            .checked_add(compressed_len)
            .ok_or(Error::Overflow("header + block size"))?;

        let write_offset = self.allocate(total_size)?;

        let bh = BlockEntryHeader {
            height,
            size: compressed_len,
            checksum: calculate_checksum(block),
            version: BLOCK_ENTRY_VERSION,
        };

        // Write the header + compressed data in one positioned write.
        let mut buf = Vec::with_capacity(total_size as usize);
        buf.extend_from_slice(&bh.marshal_binary());
        buf.extend_from_slice(&compressed);
        self.data.write_at(write_offset, &buf)?;

        self.index
            .write_index_entry(index_offset, write_offset, compressed_len)?;

        self.update_max_height(height)?;

        if let Some(cache) = &self.block_cache {
            cache.lock().put(height, block.to_vec());
        }

        Ok(())
    }

    /// Atomically reserves space for a block (Go `allocateBlockSpace`), returning
    /// the global write offset.
    fn allocate(&self, total_size: u32) -> Result<u64> {
        let max = self.max_data_file_size();
        loop {
            let current = self.next_write_offset.load(Ordering::Acquire);
            let (write_offset, new_next) = allocate_block_space(current, total_size, max)?;
            if self
                .next_write_offset
                .compare_exchange(current, new_next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(write_offset);
            }
        }
    }

    /// Updates the tracked max height (Go `updateBlockMaxHeight`) and checkpoints
    /// the header on the configured interval.
    fn update_max_height(&self, written: BlockHeight) -> Result<()> {
        loop {
            let cur = self.max_block_height.load(Ordering::Acquire);
            if cur != UNSET_HEIGHT && written <= cur {
                break;
            }
            if self
                .max_block_height
                .compare_exchange(cur, written, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        // checkpoint_interval is guaranteed non-zero by config validation.
        if written.is_multiple_of(self.config.checkpoint_interval) {
            self.persist_index_header()?;
        }
        Ok(())
    }

    fn read_block_index(&self, height: BlockHeight) -> Result<crate::format::IndexEntry> {
        let max_height = self.max_block_height.load(Ordering::Acquire);
        if max_height == UNSET_HEIGHT || height > max_height {
            return Err(Error::NotFound);
        }
        self.index.read_index_entry(height)
    }

    /// Retrieves a block by height (Go `Get`).
    ///
    /// # Errors
    /// Returns [`Error::Closed`] if closed, [`Error::NotFound`] if absent, or
    /// [`Error::ChecksumMismatch`] on a checksum failure.
    pub fn get(&self, height: BlockHeight) -> Result<BlockData> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        if let Some(cache) = &self.block_cache
            && let Some(block) = cache.lock().get(&height)
        {
            return Ok(block.clone());
        }

        let entry = self.read_block_index(height)?;
        let total_read = u64::from(SIZE_OF_BLOCK_ENTRY_HEADER)
            .checked_add(u64::from(entry.size))
            .ok_or(Error::Overflow("total read size"))?;
        let total_read =
            usize::try_from(total_read).map_err(|_| Error::Overflow("total read size"))?;
        let mut buf = vec![0u8; total_read];
        self.data.read_at(entry.offset, &mut buf)?;

        let header_len = SIZE_OF_BLOCK_ENTRY_HEADER as usize;
        let (header_bytes, data_bytes) = buf.split_at(header_len);
        let bh = BlockEntryHeader::unmarshal_binary(header_bytes)?;
        let decompressed = self.decompress(data_bytes)?;

        let calculated = calculate_checksum(&decompressed);
        if calculated != bh.checksum {
            return Err(Error::ChecksumMismatch {
                calculated,
                stored: bh.checksum,
            });
        }

        if let Some(cache) = &self.block_cache {
            cache.lock().put(height, decompressed.clone());
        }
        Ok(decompressed)
    }

    /// Checks if a block exists at `height` (Go `Has`).
    ///
    /// # Errors
    /// Returns [`Error::Closed`] if closed.
    pub fn has(&self, height: BlockHeight) -> Result<bool> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        match self.read_block_index(height) {
            Ok(_) => Ok(true),
            Err(Error::NotFound | Error::InvalidBlockHeight) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Returns the highest written block height, or `None` if empty.
    #[must_use]
    pub fn max_height(&self) -> Option<BlockHeight> {
        let h = self.max_block_height.load(Ordering::Acquire);
        if h == UNSET_HEIGHT { None } else { Some(h) }
    }

    /// fsyncs all data files covering heights `[start, end]` (Go `Sync`).
    ///
    /// # Errors
    /// Returns [`Error::Closed`] if closed.
    pub fn sync(&self, start: BlockHeight, end: BlockHeight) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        let max = self.max_data_file_size();
        let first = match self.read_block_index(start) {
            Ok(e) => crate::data::split_offset(e.offset, max)?.0,
            Err(Error::NotFound) => return Ok(()),
            Err(e) => return Err(e),
        };
        let last = match self.read_block_index(end) {
            Ok(e) => crate::data::split_offset(e.offset, max)?.0,
            Err(Error::NotFound) => return Ok(()),
            Err(e) => return Err(e),
        };
        for idx in first..=last {
            let off = idx
                .checked_mul(max)
                .ok_or(Error::Overflow("sync file offset"))?;
            self.data.sync_offset(off)?;
        }
        Ok(())
    }

    /// Flushes the header and closes all files (Go `Close`).
    ///
    /// # Errors
    /// Returns [`Error::Closed`] if already closed.
    pub fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Err(Error::Closed);
        }
        let res = self.index.persist_header(
            self.load_next_write_offset(),
            self.load_max_height(),
            self.config.sync_to_disk,
        );
        self.data.close();
        res
    }
}
