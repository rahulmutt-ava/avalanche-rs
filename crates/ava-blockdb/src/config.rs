// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Configuration for [`crate::BlockDb`] (Go `x/blockdb/config.go`).

use std::path::PathBuf;

use crate::error::{Error, Result};

/// Default maximum size of a data file in bytes (500 GiB; Go `DefaultMaxDataFileSize`).
pub const DEFAULT_MAX_DATA_FILE_SIZE: u64 = 500 * 1024 * 1024 * 1024;

/// Default maximum number of cached data-file descriptors (Go `DefaultMaxDataFiles`).
pub const DEFAULT_MAX_DATA_FILES: usize = 10;

/// Default block cache size (Go `DefaultBlockCacheSize`).
pub const DEFAULT_BLOCK_CACHE_SIZE: u16 = 256;

/// Default checkpoint interval in blocks (Go `DefaultConfig`).
pub const DEFAULT_CHECKPOINT_INTERVAL: u64 = 1024;

/// Configuration parameters for the block store (Go `DatabaseConfig`).
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// Directory where the index file is stored.
    pub index_dir: PathBuf,
    /// Directory where the data files are stored.
    pub data_dir: PathBuf,
    /// Lowest block height tracked by the database.
    pub minimum_height: u64,
    /// Maximum size of a single data file in bytes.
    pub max_data_file_size: u64,
    /// Maximum number of cached data-file descriptors.
    pub max_data_files: usize,
    /// Block cache size (number of blocks).
    pub block_cache_size: u16,
    /// How frequently (in blocks) the index header is checkpointed.
    pub checkpoint_interval: u64,
    /// Whether to fsync after each write for durability.
    pub sync_to_disk: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            index_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
            minimum_height: 0,
            max_data_file_size: DEFAULT_MAX_DATA_FILE_SIZE,
            max_data_files: DEFAULT_MAX_DATA_FILES,
            block_cache_size: DEFAULT_BLOCK_CACHE_SIZE,
            checkpoint_interval: DEFAULT_CHECKPOINT_INTERVAL,
            sync_to_disk: true,
        }
    }
}

impl DatabaseConfig {
    /// Sets both `index_dir` and `data_dir` to `dir` (Go `WithDir`).
    #[must_use]
    pub fn with_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        self.index_dir = dir.clone();
        self.data_dir = dir;
        self
    }

    /// Sets `min_height` (Go `WithMinimumHeight`).
    #[must_use]
    pub fn with_minimum_height(mut self, h: u64) -> Self {
        self.minimum_height = h;
        self
    }

    /// Sets `max_data_file_size` (Go `WithMaxDataFileSize`).
    #[must_use]
    pub fn with_max_data_file_size(mut self, size: u64) -> Self {
        self.max_data_file_size = size;
        self
    }

    /// Sets `checkpoint_interval` (Go `WithCheckpointInterval`).
    #[must_use]
    pub fn with_checkpoint_interval(mut self, interval: u64) -> Self {
        self.checkpoint_interval = interval;
        self
    }

    /// Sets `sync_to_disk` (Go `WithSyncToDisk`).
    #[must_use]
    pub fn with_sync_to_disk(mut self, sync: bool) -> Self {
        self.sync_to_disk = sync;
        self
    }

    /// Sets `block_cache_size` (Go `WithBlockCacheSize`).
    #[must_use]
    pub fn with_block_cache_size(mut self, size: u16) -> Self {
        self.block_cache_size = size;
        self
    }

    /// Validates the configuration (Go `Validate`).
    ///
    /// # Errors
    /// Returns [`Error::InvalidConfig`] if any required field is unset/zero.
    pub fn validate(&self) -> Result<()> {
        if self.index_dir.as_os_str().is_empty() {
            return Err(Error::InvalidConfig("IndexDir must be provided".into()));
        }
        if self.data_dir.as_os_str().is_empty() {
            return Err(Error::InvalidConfig("DataDir must be provided".into()));
        }
        if self.checkpoint_interval == 0 {
            return Err(Error::InvalidConfig(
                "CheckpointInterval cannot be 0".into(),
            ));
        }
        if self.max_data_files == 0 {
            return Err(Error::InvalidConfig("MaxDataFiles must be positive".into()));
        }
        if self.max_data_file_size == 0 {
            return Err(Error::InvalidConfig(
                "MaxDataFileSize must be positive".into(),
            ));
        }
        Ok(())
    }
}
