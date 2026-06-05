// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Data file (`blockdb_<n>.dat`) management (Go `x/blockdb/database.go` data
//! portions).
//!
//! Blocks live in one or more append-only data files. A single logical "global
//! offset" addresses the whole data space; it maps to `(file_index,
//! local_offset)` via `max_data_file_size`. Files are split so that no block
//! ever straddles a file boundary (the writer advances the offset to the next
//! file start when a block would not fit).

use std::fs::{File, OpenOptions};
use std::num::NonZeroUsize;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lru::LruCache;
use parking_lot::Mutex;

use crate::error::{Error, Result};

/// `format!` pattern of a data file name (Go `dataFileNameFormat`).
fn data_file_name(index: usize) -> String {
    format!("blockdb_{index}.dat")
}

/// Parses a data file name into its index, returning `None` for non-data files.
pub fn parse_data_file_index(name: &str) -> Option<usize> {
    let stem = name.strip_prefix("blockdb_")?.strip_suffix(".dat")?;
    stem.parse::<usize>().ok()
}

/// Owns the data directory, the data-file LRU handle cache, and the split logic.
pub struct DataFiles {
    data_dir: PathBuf,
    max_data_file_size: u64,
    sync_to_disk: bool,
    cache: Mutex<LruCache<usize, Arc<File>>>,
}

impl DataFiles {
    /// Creates the data directory and the handle cache (Go `initializeDataFiles`).
    pub fn open(
        data_dir: &Path,
        max_data_file_size: u64,
        max_data_files: usize,
        sync_to_disk: bool,
    ) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let cap = NonZeroUsize::new(max_data_files.max(1)).unwrap_or(NonZeroUsize::MIN);
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            max_data_file_size,
            sync_to_disk,
            cache: Mutex::new(LruCache::new(cap)),
        })
    }

    fn path(&self, index: usize) -> PathBuf {
        self.data_dir.join(data_file_name(index))
    }

    /// Returns an open handle for `file_index`, opening and caching it on miss
    /// (Go `getOrOpenDataFile`).
    pub fn get_or_open(&self, file_index: usize) -> Result<Arc<File>> {
        let mut cache = self.cache.lock();
        if let Some(handle) = cache.get(&file_index) {
            return Ok(Arc::clone(handle));
        }
        let handle = Arc::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.path(file_index))?,
        );
        cache.put(file_index, Arc::clone(&handle));
        Ok(handle)
    }

    /// Maps a global offset to `(file_handle, local_offset, file_index)`
    /// (Go `getDataFileAndOffset`).
    pub fn file_and_offset(&self, global_offset: u64) -> Result<(Arc<File>, u64, usize)> {
        let (file_index_u64, local_offset) = split_offset(global_offset, self.max_data_file_size)?;
        let file_index =
            usize::try_from(file_index_u64).map_err(|_| Error::Overflow("file index"))?;
        let handle = self.get_or_open(file_index)?;
        Ok((handle, local_offset, file_index))
    }

    /// Writes `bytes` at `global_offset`, fsync'ing the file when `sync_to_disk`
    /// is set (Go `writeBlockAt`). Uses positioned writes (safe `pwrite`).
    pub fn write_at(&self, global_offset: u64, bytes: &[u8]) -> Result<()> {
        let (handle, local_offset, _) = self.file_and_offset(global_offset)?;
        handle.write_all_at(bytes, local_offset)?;
        if self.sync_to_disk {
            handle.sync_all()?;
        }
        Ok(())
    }

    /// Reads exactly `buf.len()` bytes starting at `global_offset` (positioned
    /// `pread`).
    pub fn read_at(&self, global_offset: u64, buf: &mut [u8]) -> Result<()> {
        let (handle, local_offset, _) = self.file_and_offset(global_offset)?;
        handle.read_exact_at(buf, local_offset)?;
        Ok(())
    }

    /// fsyncs the data file containing `global_offset`.
    pub fn sync_offset(&self, global_offset: u64) -> Result<()> {
        let (handle, _, _) = self.file_and_offset(global_offset)?;
        handle.sync_all()?;
        Ok(())
    }

    /// Lists existing data files, returning `(index -> path)` and the max index
    /// found (or `None` if there are none) (Go `listDataFiles`).
    pub fn list(&self) -> Result<(std::collections::BTreeMap<usize, PathBuf>, Option<usize>)> {
        let mut files = std::collections::BTreeMap::new();
        let mut max_index: Option<usize> = None;
        for entry in std::fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(idx) = parse_data_file_index(name) {
                files.insert(idx, self.data_dir.join(name));
                max_index = Some(max_index.map_or(idx, |m| m.max(idx)));
            }
        }
        Ok((files, max_index))
    }

    /// Returns the on-disk size in bytes of data file `index`.
    pub fn file_size(&self, index: usize) -> Result<u64> {
        Ok(std::fs::metadata(self.path(index))?.len())
    }

    /// Closes all cached handles (handles drop on eviction / clear).
    pub fn close(&self) {
        self.cache.lock().clear();
    }
}

/// Computes the global write offset for a block of `total_size` bytes given the
/// current `next_write_offset`, applying file-split semantics (Go
/// `allocateBlockSpace`). Returns `(write_offset, new_next_write_offset)`.
///
/// # Errors
/// Returns [`Error::BlockTooLarge`] if a single block exceeds the max file size,
/// or [`Error::Overflow`] on arithmetic overflow.
pub fn allocate_block_space(
    next_write_offset: u64,
    total_size: u32,
    max_data_file_size: u64,
) -> Result<(u64, u64)> {
    let total = u64::from(total_size);
    if total > max_data_file_size {
        return Err(Error::BlockTooLarge);
    }

    let current = next_write_offset;
    let block_end = current
        .checked_add(total)
        .ok_or(Error::Overflow("offset + block size"))?;

    let mut write_offset = current;
    let mut block_end_offset = block_end;

    // `max_data_file_size > 0` is guaranteed by config validation; the Go code
    // still guards it, so we mirror that with checked division.
    if max_data_file_size > 0 {
        let (current_file_index, offset_within) = split_offset(current, max_data_file_size)?;
        let end_within = offset_within
            .checked_add(total)
            .ok_or(Error::Overflow("block end within file"))?;
        if end_within > max_data_file_size {
            let next_file_index = current_file_index
                .checked_add(1)
                .ok_or(Error::Overflow("file index"))?;
            write_offset = next_file_index
                .checked_mul(max_data_file_size)
                .ok_or(Error::Overflow("next file offset"))?;
            block_end_offset = write_offset
                .checked_add(total)
                .ok_or(Error::Overflow("new file offset + block size"))?;
        }
    }

    Ok((write_offset, block_end_offset))
}

/// Splits a global offset into `(file_index, local_offset)` using checked
/// division (the file size is guaranteed non-zero by config validation).
///
/// # Errors
/// Returns [`Error::Overflow`] if `max_data_file_size` is zero.
pub fn split_offset(global_offset: u64, max_data_file_size: u64) -> Result<(u64, u64)> {
    let file_index = global_offset
        .checked_div(max_data_file_size)
        .ok_or(Error::Overflow("offset / file size"))?;
    let local_offset = global_offset
        .checked_rem(max_data_file_size)
        .ok_or(Error::Overflow("offset % file size"))?;
    Ok((file_index, local_offset))
}
