// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init steps 11–12 (specs/12 §2.2): open the node database (mirror Go
//! `initDatabase` — genesis-hash check + `ungracefulShutdown` marker) and the
//! cross-chain shared memory (mirror Go `initSharedMemory`).

use std::sync::Arc;

use ava_chains::atomic::Memory;
use ava_config::node::{Config, DatabaseConfig};
use ava_database::{
    Batch, BoxIter, Compacter, Database, DynDatabase, Iteratee, KeyValueDeleter, KeyValueReader,
    KeyValueWriter, MemDb, MeterDb, PrefixDb,
};
use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::init::metrics::NodeMetrics;

/// Go `genesisHashKey`.
pub const GENESIS_HASH_KEY: &[u8] = b"genesisID";
/// Go `ungracefulShutdown` marker key — written at init, deleted by the
/// graceful-shutdown path (M8.30, 12 §2.4 step 13).
pub const UNGRACEFUL_SHUTDOWN_KEY: &[u8] = b"ungracefulShutdown";
/// Go `indexerDBPrefix`.
pub const INDEXER_DB_PREFIX: &[u8] = &[0x00];
/// Go's shared-memory prefix (`prefixdb.New([]byte("shared memory"), n.DB)`).
pub const SHARED_MEMORY_PREFIX: &[u8] = b"shared memory";

/// An object-safe view of the node database that still implements the typed
/// [`Database`] trait (whose iterator GAT is not object-safe), so generic
/// wrappers ([`PrefixDb`], the indexer) can run over the dynamically-chosen
/// backend without making `Node` generic.
#[derive(Clone)]
pub struct DynDb(Arc<dyn DynDatabase>);

impl DynDb {
    /// Wrap a shared dynamic database handle.
    #[must_use]
    pub fn new(db: Arc<dyn DynDatabase>) -> Self {
        Self(db)
    }
}

/// A boxed iterator satisfying the [`Iteratee`] GAT for [`DynDb`].
pub struct DynIter<'a>(BoxIter<'a>);

impl ava_database::Iterator for DynIter<'_> {
    fn next(&mut self) -> bool {
        self.0.next()
    }
    fn error(&self) -> ava_database::Result<()> {
        self.0.error()
    }
    fn key(&self) -> Option<&[u8]> {
        self.0.key()
    }
    fn value(&self) -> Option<&[u8]> {
        self.0.value()
    }
    fn release(&mut self) {
        self.0.release();
    }
}

impl KeyValueReader for DynDb {
    fn has(&self, key: &[u8]) -> ava_database::Result<bool> {
        self.0.has(key)
    }
    fn get(&self, key: &[u8]) -> ava_database::Result<Vec<u8>> {
        self.0.get(key)
    }
}

impl KeyValueWriter for DynDb {
    fn put(&self, key: &[u8], value: &[u8]) -> ava_database::Result<()> {
        self.0.put(key, value)
    }
}

impl KeyValueDeleter for DynDb {
    fn delete(&self, key: &[u8]) -> ava_database::Result<()> {
        self.0.delete(key)
    }
}

impl ava_database::Batcher for DynDb {
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        self.0.new_batch()
    }
}

impl Iteratee for DynDb {
    type Iter<'a> = DynIter<'a>;

    fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> Self::Iter<'_> {
        DynIter(self.0.new_iterator_with_start_and_prefix(start, prefix))
    }
}

impl Compacter for DynDb {
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> ava_database::Result<()> {
        self.0.compact(start, limit)
    }
}

impl Database for DynDb {
    fn close(&self) -> ava_database::Result<()> {
        self.0.close()
    }
    fn health_check(&self) -> ava_database::Result<serde_json::Value> {
        self.0.health_check()
    }
}

/// The Go on-disk folder name for the configured backend (`initDatabase`):
/// `leveldb` data lives under the database version (`v1.4.5`), `pebbledb`
/// under `pebble`, anything else under `db`.
fn db_folder_name(name: &str) -> &'static str {
    match name {
        "leveldb" => ava_version::CURRENT_DATABASE,
        "pebbledb" => "pebble",
        _ => "db",
    }
}

/// Open the configured backend. `memdb` is in-memory; every on-disk name
/// (`leveldb` / `pebbledb` / `rocksdb`) opens the RocksDB backend (the Rust
/// node's single on-disk engine, 04 §2.1) under the Go-compatible folder.
fn open_backend(
    db_config: &DatabaseConfig,
    meter_registry: &prometheus::Registry,
) -> Result<Arc<dyn DynDatabase>> {
    if db_config.name == "memdb" {
        let metered = MeterDb::new(meter_registry, MemDb::new()).map_err(Error::Database)?;
        return Ok(Arc::new(metered));
    }

    #[cfg(feature = "rocksdb")]
    {
        let full_path = std::path::Path::new(&db_config.path).join(db_folder_name(&db_config.name));
        let db = ava_database::RocksDb::open(full_path).map_err(Error::Database)?;
        let metered = MeterDb::new(meter_registry, db).map_err(Error::Database)?;
        Ok(Arc::new(metered))
    }
    #[cfg(not(feature = "rocksdb"))]
    {
        let _ = db_folder_name(&db_config.name);
        Err(Error::DatabaseInit(format!(
            "on-disk database {:?} requires the `rocksdb` feature (enabled in release builds)",
            db_config.name
        )))
    }
}

/// Step 11: open + verify the node database (mirror Go `initDatabase`).
///
/// Wraps the backend in [`MeterDb`] (registered under the `all` label of the
/// meterdb gatherer, like Go), pins the genesis hash on first run and verifies
/// it on every subsequent run, then writes the [`UNGRACEFUL_SHUTDOWN_KEY`]
/// marker (warning if the previous run never deleted it).
///
/// # Errors
/// - Backend open / metrics registration failures.
/// - [`Error::GenesisHashMismatch`] when the DB belongs to another genesis.
pub fn init_database(config: &Config, metrics: &NodeMetrics) -> Result<Arc<dyn DynDatabase>> {
    let _db_registry = ava_api::metrics::make_and_register(
        metrics.gatherer.as_ref(),
        &crate::init::namespace::db(),
    )?;
    let meter_registry = ava_api::metrics::make_and_register(metrics.meter_db.as_ref(), "all")?;

    if config.database_config.read_only {
        // The Rust backend has no read-only open yet (deferral,
        // `tests/PORTING.md`).
        tracing::warn!("--db-read-only is not supported yet; opening read-write");
    }

    // Pre-open guard (26 §6 / 04 §11): refuse a foreign/older Go data dir
    // (Pebble / PREV_DATABASE) rather than opening it in place and corrupting
    // it. Runs strictly before the open, so it never touches the
    // `ungracefulShutdown` marker written below.
    crate::init::db_init::precheck_data_dir(&config.database_config)?;

    let db = open_backend(&config.database_config, &meter_registry)?;

    let expected_genesis_hash = ava_crypto::hashing::sha256(&config.genesis_bytes);
    let raw_genesis_hash = match db.get(GENESIS_HASH_KEY) {
        Ok(bytes) => bytes,
        Err(ava_database::Error::NotFound) => {
            db.put(GENESIS_HASH_KEY, &expected_genesis_hash)
                .map_err(Error::Database)?;
            expected_genesis_hash.to_vec()
        }
        Err(e) => return Err(Error::Database(e)),
    };

    let genesis_hash = Id::from_slice(&raw_genesis_hash)
        .map_err(|e| Error::DatabaseInit(format!("invalid persisted genesis hash: {e}")))?;
    let expected_genesis = Id::from(expected_genesis_hash);
    if genesis_hash != expected_genesis {
        return Err(Error::GenesisHashMismatch {
            db_genesis: genesis_hash,
            expected_genesis,
        });
    }

    tracing::info!(genesis_hash = %genesis_hash, "initializing database");

    let ungraceful = db.has(UNGRACEFUL_SHUTDOWN_KEY).map_err(Error::Database)?;
    if ungraceful {
        tracing::warn!("detected previous ungraceful shutdown");
    }
    db.put(UNGRACEFUL_SHUTDOWN_KEY, &[])
        .map_err(Error::Database)?;

    Ok(db)
}

/// Step 12: cross-chain shared memory over a `"shared memory"`-prefixed view
/// of the node DB (mirror Go `initSharedMemory`).
#[must_use]
pub fn init_shared_memory(db: &Arc<dyn DynDatabase>) -> Arc<Memory> {
    tracing::info!("initializing SharedMemory");
    let prefixed: Arc<dyn DynDatabase> = Arc::new(PrefixDb::new(
        SHARED_MEMORY_PREFIX,
        DynDb::new(Arc::clone(db)),
    ));
    Memory::new(prefixed)
}
