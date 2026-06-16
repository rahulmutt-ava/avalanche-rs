// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The rolling-upgrade **swap / import** orchestration (specs/02 §10.4, specs/26
//! §7, specs/00 §4.4).
//!
//! Models an N-node network that starts on the previous-released **Go** binary
//! and is rolled, one node at a time, onto the **Rust** binary across an
//! activation height. Each swap runs the M9.16 Go-dir → RocksDB import path for
//! real: it drives [`ava_database::migrate::import_source_into_rocksdb`] over an
//! injected [`GoDbSource`](ava_database::migrate::GoDbSource), then **re-opens**
//! the imported `v1.4.5/` RocksDB dir and asserts the migrated KV set is
//! byte-identical to the source (continuity of state across the cut-over).
//!
//! The Go data dir of each node is modelled by a [`GoNodeData`] — an in-memory
//! [`GoDbSource`] mirroring the §10 layout (verbatim `(key, value)` pairs in
//! lexicographic order). The orchestration is binary-agnostic: it is the same
//! whether a node's source bytes came from a live previous-Go node (live arm) or
//! a synthetic fixture (offline arm).

use std::path::Path;

use ava_database::migrate::GoDbSource;
use ava_database::migrate::import::{
    GoBackend, ImportOptions, ImportReport, import_source_into_rocksdb,
};
use ava_database::rocksdb::RocksDb;
use ava_database::traits::KeyValueReader;

/// A rolling-upgrade orchestration / import failure.
#[derive(Debug, thiserror::Error)]
pub enum SwapError {
    /// A node index outside `0..nodes` was passed to [`RollingUpgrade::swap`].
    #[error("node index {index} out of range (network has {nodes} nodes)")]
    NodeOutOfRange {
        /// The offending node index.
        index: usize,
        /// The number of nodes in the network.
        nodes: usize,
    },

    /// The node was already rolled onto Rust; a second swap is a planning bug.
    #[error("node {index} was already swapped to Rust")]
    AlreadySwapped {
        /// The node index that was double-swapped.
        index: usize,
    },

    /// The Go-dir → RocksDB import facade failed.
    #[error("go-dir import for node {index}: {source}")]
    Import {
        /// The node whose import failed.
        index: usize,
        /// The underlying import-facade error.
        source: ava_database::migrate::import::ImportError,
    },

    /// Re-opening the imported RocksDB dir to verify continuity failed.
    #[error("re-open imported rocksdb dir for node {index}: {source}")]
    ReopenRocksDb {
        /// The node whose re-open failed.
        index: usize,
        /// The underlying RocksDB error.
        source: ava_database::error::Error,
    },

    /// The imported KV set did not byte-match the source — a state discontinuity
    /// across the swap. This MUST never happen on a clean import; it is the
    /// load-bearing continuity assertion of the swap step.
    #[error(
        "state discontinuity importing node {index}: key {key:?} \
         expected {expected:?}, imported dir has {found:?}"
    )]
    Discontinuity {
        /// The node whose imported state diverged from its source.
        index: usize,
        /// The key that diverged.
        key: Vec<u8>,
        /// The source value (Go bytes).
        expected: Vec<u8>,
        /// The value read back from the imported RocksDB dir (`None` = missing).
        found: Option<Vec<u8>>,
    },

    /// A temp-dir / filesystem failure.
    #[error("filesystem: {0}")]
    Io(#[from] std::io::Error),
}

/// The implementation a node slot is currently running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Running {
    /// The previous-released Go `avalanchego` binary (pre-swap).
    Go,
    /// The Rust `avalanchers` binary (post-swap, after Go-dir import).
    Rust,
}

/// One node's modelled Go data dir: every `(key, value)` pair in the node's base
/// DB, verbatim, in the §10 layout. Backs a [`GoDbSource`] for the import.
#[derive(Debug, Clone, Default)]
pub struct GoNodeData {
    pairs: Vec<(Vec<u8>, Vec<u8>)>,
}

impl GoNodeData {
    /// Build a node data dir from `(key, value)` pairs (order-independent; the
    /// [`GoDbSource`] yields them in lexicographic key order, matching the §11.4
    /// source contract).
    #[must_use]
    pub fn from_pairs<K, V, I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<Vec<u8>>,
        V: Into<Vec<u8>>,
    {
        Self {
            pairs: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// The number of `(key, value)` pairs in this node's data dir.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Whether this node's data dir is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// The source pairs, sorted by key (the canonical import order).
    #[must_use]
    fn sorted(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut pairs = self.pairs.clone();
        pairs.sort();
        pairs
    }
}

impl GoDbSource for GoNodeData {
    fn iter_all(&self) -> anyhow::Result<Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)>>> {
        Ok(Box::new(self.sorted().into_iter()))
    }
}

/// One node in the rolling upgrade: its modelled Go data dir + which binary it is
/// currently running.
#[derive(Debug, Clone)]
pub struct Node {
    /// The node's Go base-DB contents (the import source on swap).
    pub data: GoNodeData,
    /// The binary the node is currently running.
    pub running: Running,
}

/// The outcome of one node's swap: the import report plus the byte-verified pair
/// count read back from the re-opened RocksDB dir.
#[derive(Debug)]
pub struct SwapReport {
    /// The node that was swapped.
    pub index: usize,
    /// The import-facade report (pairs copied, dst dir, verify tier).
    pub import: ImportReport,
    /// The number of pairs read back and byte-verified from the imported dir.
    pub verified_pairs: u64,
}

/// An N-node rolling-upgrade plan: all nodes start on Go and are swapped to Rust
/// one at a time (specs/02 §10.4). Each [`swap`](Self::swap) runs the real
/// Go-dir → RocksDB import and verifies state continuity.
#[derive(Debug)]
pub struct RollingUpgrade {
    nodes: Vec<Node>,
}

impl RollingUpgrade {
    /// Start an N-node network on the previous-released Go binary, each node
    /// holding the given Go data dir.
    #[must_use]
    pub fn start_on_go(datas: Vec<GoNodeData>) -> Self {
        let nodes = datas
            .into_iter()
            .map(|data| Node {
                data,
                running: Running::Go,
            })
            .collect();
        Self { nodes }
    }

    /// The number of nodes in the network.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the network has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The binary node `index` is currently running, or `None` if out of range.
    #[must_use]
    pub fn running(&self, index: usize) -> Option<Running> {
        self.nodes.get(index).map(|n| n.running)
    }

    /// Whether every node has been rolled onto the Rust binary.
    #[must_use]
    pub fn all_rust(&self) -> bool {
        self.nodes.iter().all(|n| n.running == Running::Rust)
    }

    /// Swap node `index` from Go to Rust: import its Go data dir into a fresh
    /// RocksDB dir under `dst_root` (the M9.16 facade, REAL on-disk write path),
    /// re-open that dir, and assert the imported KV set is byte-identical to the
    /// source — the load-bearing **continuity-of-state** check for the cut-over.
    ///
    /// `dst_root` is a fresh per-node destination directory (the caller owns its
    /// lifetime; in tests a `tempfile::TempDir`).
    ///
    /// # Errors
    ///
    /// - [`SwapError::NodeOutOfRange`] / [`SwapError::AlreadySwapped`] on a
    ///   planning bug.
    /// - [`SwapError::Import`] if the import facade fails.
    /// - [`SwapError::ReopenRocksDb`] if the imported dir cannot be re-opened.
    /// - [`SwapError::Discontinuity`] if a migrated value does not byte-match the
    ///   source (state was lost or mangled across the swap).
    pub fn swap(&mut self, index: usize, dst_root: &Path) -> Result<SwapReport, SwapError> {
        let nodes = self.nodes.len();
        let node = self
            .nodes
            .get_mut(index)
            .ok_or(SwapError::NodeOutOfRange { index, nodes })?;
        if node.running == Running::Rust {
            return Err(SwapError::AlreadySwapped { index });
        }

        // (a) Run the REAL Go-dir → RocksDB import facade over the node's source.
        // Goleveldb backend: the source pairs are the verbatim §10 catalog; the
        // import writes a fresh `v1.4.5/` RocksDB dir under `dst_root`.
        let opts = ImportOptions::default();
        let report = import_source_into_rocksdb(&node.data, dst_root, GoBackend::Goleveldb, &opts)
            .map_err(|source| SwapError::Import { index, source })?;

        // (b) Re-open the imported RocksDB dir and assert the KV set is
        // byte-identical to the source — continuity of state across the swap.
        let db = RocksDb::open(&report.dst_dir)
            .map_err(|source| SwapError::ReopenRocksDb { index, source })?;

        let mut verified: u64 = 0;
        for (key, expected) in node.data.sorted() {
            match KeyValueReader::get(&db, &key) {
                Ok(found) if found == expected => {
                    verified = verified.saturating_add(1);
                }
                Ok(found) => {
                    return Err(SwapError::Discontinuity {
                        index,
                        key,
                        expected,
                        found: Some(found),
                    });
                }
                Err(ava_database::error::Error::NotFound) => {
                    return Err(SwapError::Discontinuity {
                        index,
                        key,
                        expected,
                        found: None,
                    });
                }
                Err(source) => {
                    return Err(SwapError::ReopenRocksDb { index, source });
                }
            }
        }

        node.running = Running::Rust;
        Ok(SwapReport {
            index,
            import: report,
            verified_pairs: verified,
        })
    }
}
