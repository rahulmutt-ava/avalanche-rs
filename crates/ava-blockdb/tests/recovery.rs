// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Torn-write recovery tests for `ava-blockdb` (specs/27 §4.1/§5.1).

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use ava_blockdb::format::{IndexEntry, IndexFileHeader};
use ava_blockdb::{BlockDb, DatabaseConfig};

const INDEX_FILE: &str = "blockdb.idx";

fn config(dir: &Path) -> DatabaseConfig {
    DatabaseConfig::default()
        .with_dir(dir)
        .with_max_data_file_size(10 * 1024)
        .with_block_cache_size(0)
        // A large checkpoint interval so the header is NOT updated per block;
        // this is what creates the "index behind data" torn-write window.
        .with_checkpoint_interval(1_000_000)
        .with_sync_to_disk(true)
}

fn read_header(index_path: &Path) -> IndexFileHeader {
    let f = fs::OpenOptions::new().read(true).open(index_path).unwrap();
    let mut buf = [0u8; 64];
    f.read_exact_at(&mut buf, 0).unwrap();
    IndexFileHeader::unmarshal_binary(&buf).unwrap()
}

fn write_header(index_path: &Path, header: &IndexFileHeader) {
    let f = fs::OpenOptions::new().write(true).open(index_path).unwrap();
    f.write_all_at(&header.marshal_binary(), 0).unwrap();
}

/// Writes N blocks, then forcibly rewinds the index header so it only "knows"
/// about the first block (simulating a crash before later index/header writes
/// were persisted). On reopen the recovery scan must rebuild the index so every
/// block reads back identically.
#[test]
fn recovery_rebuilds_index() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let index_path = dir.join(INDEX_FILE);

    let num_blocks = 6u64;
    let mut blocks = Vec::new();
    {
        let db = BlockDb::open(config(dir)).unwrap();
        for h in 0..num_blocks {
            let block: Vec<u8> = (0..2048u32)
                .map(|i| (i.wrapping_add(h as u32)) as u8)
                .collect();
            db.put(h, &block).unwrap();
            blocks.push(block);
        }
        // Persist a *correct* header first so we can read the real offsets, then
        // we will rewind it below to simulate the torn write.
        db.close().unwrap();
    }

    // Capture the real (fully-recovered) header so we know the true end offset.
    let good_header = read_header(&index_path);
    assert_ne!(good_header.next_write_offset, 0);

    // Read the first block's index entry to learn where block 0 ends.
    let first_entry = {
        let f = fs::OpenOptions::new().read(true).open(&index_path).unwrap();
        let mut buf = [0u8; 16];
        f.read_exact_at(&mut buf, 64).unwrap(); // first entry right after header
        IndexEntry::unmarshal_binary(&buf).unwrap()
    };
    let first_block_end = 22 + u64::from(first_entry.size); // entry header + compressed size

    // Rewind the header to only know about block 0, and zero out every index
    // slot beyond block 0 to simulate them never having been written.
    let mut torn = good_header;
    torn.next_write_offset = first_block_end;
    torn.max_height = 0;
    write_header(&index_path, &torn);
    {
        let f = fs::OpenOptions::new()
            .write(true)
            .open(&index_path)
            .unwrap();
        let empty = IndexEntry::default().marshal_binary();
        for h in 1..num_blocks {
            let off = 64 + h * 16;
            f.write_all_at(&empty, off).unwrap();
        }
        f.sync_all().unwrap();
    }

    // Reopen: recovery should scan forward and rebuild all index entries.
    let db = BlockDb::open(config(dir)).unwrap();
    for (h, expected) in blocks.iter().enumerate() {
        let got = db.get(h as u64).expect("get after recovery");
        assert_eq!(&got, expected, "block {h} mismatch after recovery");
    }
    assert_eq!(db.max_height(), Some(num_blocks - 1));

    // The rebuilt header must match the originally-correct end offset.
    db.close().unwrap();
    let recovered_header = read_header(&index_path);
    assert_eq!(
        recovered_header.next_write_offset, good_header.next_write_offset,
        "recovered next_write_offset must match the original",
    );
    assert_eq!(recovered_header.max_height, good_header.max_height);
}

/// A missing data file in the middle of the sequence is unrecoverable corruption
/// (Go `TestDataSplitting_DeletedFile`).
#[test]
fn recovery_errors_on_missing_data_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // Small files so multiple .dat files are produced. Use incompressible
    // (pseudo-random) block data so zstd does not shrink it below the split
    // threshold.
    let cfg = DatabaseConfig::default()
        .with_dir(dir)
        .with_max_data_file_size(4096)
        .with_block_cache_size(0);
    {
        let db = BlockDb::open(cfg.clone()).unwrap();
        for h in 0..5u64 {
            // A simple LCG produces incompressible-enough bytes.
            let mut state = h.wrapping_add(1);
            let block: Vec<u8> = (0..1500u32)
                .map(|_| {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    (state >> 33) as u8
                })
                .collect();
            db.put(h, &block).unwrap();
        }
        db.close().unwrap();
    }
    // Confirm the writes actually split across at least two data files.
    let count = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("blockdb_") && n.ends_with(".dat"))
        })
        .count();
    assert!(count >= 2, "expected multiple data files, got {count}");
    // Delete the first data file.
    fs::remove_file(dir.join("blockdb_0.dat")).unwrap();
    match BlockDb::open(cfg) {
        Ok(_) => panic!("expected Corrupted error, got Ok"),
        Err(ava_blockdb::Error::Corrupted) => {}
        Err(e) => panic!("expected Corrupted, got {e:?}"),
    }
}
