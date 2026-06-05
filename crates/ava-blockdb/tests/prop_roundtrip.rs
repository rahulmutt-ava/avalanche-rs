// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Round-trip property test for `ava-blockdb`.

use std::collections::BTreeMap;

use ava_blockdb::{BlockDb, DatabaseConfig};
use proptest::prelude::*;

fn open_db(dir: &std::path::Path, max_data_file_size: u64, sync: bool) -> BlockDb {
    let config = DatabaseConfig::default()
        .with_dir(dir)
        .with_max_data_file_size(max_data_file_size)
        // Disable the block cache so reads exercise the on-disk path.
        .with_block_cache_size(0)
        .with_sync_to_disk(sync);
    BlockDb::open(config).expect("open blockdb")
}

proptest! {
    // (height, block_bytes) pairs at arbitrary, possibly out-of-order heights.
    #[test]
    fn blockdb_roundtrip(
        entries in prop::collection::vec(
            (0u64..200, prop::collection::vec(any::<u8>(), 1..2048)),
            1..40,
        ),
        sync in any::<bool>(),
    ) {
        let tmp = tempfile::tempdir().unwrap();
        // Small data files force splitting across many .dat files.
        let db = open_db(tmp.path(), 8 * 1024, sync);

        // Dedup by height: last write at a height wins (Go behavior on overwrite).
        let mut oracle: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        for (h, block) in &entries {
            db.put(*h, block).expect("put");
            oracle.insert(*h, block.clone());
        }

        // Every written height reads back byte-identical.
        for (h, expected) in &oracle {
            let got = db.get(*h).expect("get");
            prop_assert_eq!(&got, expected, "mismatch at height {}", h);
            prop_assert!(db.has(*h).unwrap());
        }

        // max_height equals the largest written height.
        prop_assert_eq!(db.max_height(), oracle.keys().next_back().copied());

        // Reopen and verify everything survives a clean close.
        db.close().expect("close");
        let db2 = open_db(tmp.path(), 8 * 1024, sync);
        for (h, expected) in &oracle {
            let got = db2.get(*h).expect("get after reopen");
            prop_assert_eq!(&got, expected, "post-reopen mismatch at height {}", h);
        }
        prop_assert_eq!(db2.max_height(), oracle.keys().next_back().copied());
        db2.close().expect("close 2");
    }
}
