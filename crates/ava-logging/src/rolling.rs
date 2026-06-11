// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Lumberjack-equivalent size-aware rolling file writer (specs/18 §5.3).
//!
//! Mirrors Go's `gopkg.in/natefinch/lumberjack.v2`, which avalanchego's
//! `utils/logging/factory.go` uses as the per-logger rotating writer. The four
//! knobs on [`Rotation`](crate::Rotation) map 1:1 onto lumberjack:
//!
//! - `max_size_mib` (`MaxSize`): the active file is rotated once a write would
//!   push it past this many MiB.
//! - `max_files` (`MaxBackups`): at most this many timestamped backups are kept;
//!   older backups are pruned.
//! - `max_age_days` (`MaxAge`): backups older than this are dropped (`0` keeps
//!   them indefinitely).
//! - `compress` (`Compress`): rotated backups are gzip-compressed (`.log.gz`).
//!
//! The live file always keeps the stable name `<name>.log` (matching Go, where
//! greps/tails follow a fixed path). On rotation the active file is renamed to a
//! timestamped backup using lumberjack's `<name>-<timestamp>.log` layout
//! (timestamp `2006-01-02T15-04-05.000`), then a fresh `<name>.log` is opened.
//!
//! Rotation, pruning and compression all run synchronously on the writing
//! thread at rotate time — there is no background goroutine/thread as lumberjack
//! has, but the behavior (observable filenames + retention) matches. The writer
//! is wrapped by `tracing-appender`'s `NonBlocking` in [`crate`], so the actual
//! `write` calls happen off the hot path regardless.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::Rotation;

/// lumberjack's backup timestamp layout (`backupTimeFormat`), `chrono`-spelled.
const BACKUP_TIME_FORMAT: &str = "%Y-%m-%dT%H-%M-%S%.3f";

/// One mebibyte, the unit `max_size_mib` is expressed in (lumberjack `megabyte`).
const MIB: u64 = 1024 * 1024;

/// A size-aware rolling file writer honoring lumberjack's rotation knobs.
///
/// Construct with [`RollingWriter::new`]; it implements [`Write`] so it can be
/// handed to `tracing-appender`'s `non_blocking`. All rotation bookkeeping runs
/// on the writing thread inside [`Write::write`].
pub(crate) struct RollingWriter {
    /// Directory holding the live file and its backups.
    dir: PathBuf,
    /// Logger name (file stem), e.g. `main` or a chain alias.
    name: String,
    /// Rotation policy.
    rotation: Rotation,
    /// The currently open live file (`<dir>/<name>.log`), if opened.
    file: Option<File>,
    /// Bytes written to the current live file so far.
    size: u64,
}

impl RollingWriter {
    /// Open (or create) the live file `<dir>/<name>.log` and return a writer.
    ///
    /// # Errors
    /// Propagates any I/O error creating the directory or opening the file.
    pub(crate) fn new(dir: &Path, name: &str, rotation: Rotation) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let mut writer = Self {
            dir: dir.to_path_buf(),
            name: name.to_owned(),
            rotation,
            file: None,
            size: 0,
        };
        writer.open_existing_or_create()?;
        Ok(writer)
    }

    /// Path of the live (active) file.
    fn live_path(&self) -> PathBuf {
        self.dir.join(format!("{}.log", self.name))
    }

    /// Maximum live-file size in bytes (`max_size_mib` MiB).
    fn max_size_bytes(&self) -> u64 {
        u64::from(self.rotation.max_size_mib).saturating_mul(MIB)
    }

    /// Open the existing live file (appending, picking up its size) or create a
    /// fresh one.
    fn open_existing_or_create(&mut self) -> io::Result<()> {
        let path = self.live_path();
        let existing_len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        self.file = Some(file);
        self.size = existing_len;
        Ok(())
    }

    /// Rotate the live file: rename it to a timestamped backup, open a fresh
    /// live file, then compress (if configured) and prune backups.
    fn rotate(&mut self) -> io::Result<()> {
        // Drop the current handle so the rename succeeds on all platforms.
        self.file = None;

        let live = self.live_path();
        if fs::metadata(&live).map(|m| m.len()).unwrap_or(0) > 0 {
            let backup = self.backup_path(Local::now());
            fs::rename(&live, &backup)?;
            if self.rotation.compress {
                // Best-effort compression: if it fails, keep the plain backup.
                let _ = compress_file(&backup);
            }
        }

        self.open_existing_or_create()?;
        self.prune()?;
        Ok(())
    }

    /// The timestamped backup path for `when` (lumberjack `<name>-<ts>.log`).
    fn backup_path(&self, when: DateTime<Local>) -> PathBuf {
        let ts = when.format(BACKUP_TIME_FORMAT).to_string();
        self.dir.join(format!("{}-{}.log", self.name, ts))
    }

    /// Drop backups beyond `max_files` (oldest first) and those older than
    /// `max_age_days`.
    fn prune(&self) -> io::Result<()> {
        let mut backups = self.list_backups()?;
        // Newest first by embedded timestamp.
        backups.sort_by_key(|b| std::cmp::Reverse(b.timestamp));

        let max_files = self.rotation.max_files as usize;
        let cutoff = if self.rotation.max_age_days == 0 {
            None
        } else {
            // Backup timestamps are parsed as naive local times; compare against
            // a naive `now` to match. `checked_sub_signed` avoids the panicking
            // `-` operator (and the `arithmetic_side_effects` lint).
            Local::now()
                .naive_local()
                .checked_sub_signed(chrono::Duration::days(i64::from(
                    self.rotation.max_age_days,
                )))
        };

        for (index, backup) in backups.iter().enumerate() {
            let over_count = max_files != 0 && index >= max_files;
            let too_old = cutoff.is_some_and(|c| backup.timestamp < c);
            if over_count || too_old {
                let _ = fs::remove_file(&backup.path);
            }
        }
        Ok(())
    }

    /// All backups in `dir` belonging to this logger, with parsed timestamps.
    fn list_backups(&self) -> io::Result<Vec<Backup>> {
        let prefix = format!("{}-", self.name);
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            // Accept both `<ts>.log` and `<ts>.log.gz`.
            let ts_str = rest
                .strip_suffix(".log.gz")
                .or_else(|| rest.strip_suffix(".log"));
            let Some(ts_str) = ts_str else {
                continue;
            };
            let Ok(timestamp) = chrono::NaiveDateTime::parse_from_str(ts_str, BACKUP_TIME_FORMAT)
            else {
                continue;
            };
            out.push(Backup {
                path: entry.path(),
                timestamp,
            });
        }
        Ok(out)
    }
}

/// A discovered backup file plus its parsed creation timestamp.
struct Backup {
    path: PathBuf,
    timestamp: chrono::NaiveDateTime,
}

/// Gzip `path` in place, replacing it with `<path>.gz` and removing the original.
fn compress_file(path: &Path) -> io::Result<()> {
    let gz_path = {
        let mut s = path.as_os_str().to_owned();
        s.push(".gz");
        PathBuf::from(s)
    };
    let input = fs::read(path)?;
    let out = File::create(&gz_path)?;
    let mut encoder = GzEncoder::new(out, Compression::default());
    encoder.write_all(&input)?;
    encoder.finish()?;
    fs::remove_file(path)?;
    Ok(())
}

impl Write for RollingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let write_len = buf.len() as u64;
        let max = self.max_size_bytes();
        // Rotate before this write would push us past the cap (lumberjack
        // rotates when the new write does not fit), but never on an empty file
        // (a single record larger than max still has to land somewhere).
        if max > 0 && self.size > 0 && self.size.saturating_add(write_len) > max {
            self.rotate()?;
        }

        let file = match self.file.as_mut() {
            Some(f) => f,
            None => {
                self.open_existing_or_create()?;
                // open_existing_or_create always sets `file`; re-borrow.
                self.file
                    .as_mut()
                    .ok_or_else(|| io::Error::other("rolling writer file unavailable"))?
            }
        };
        let written = file.write(buf)?;
        self.size = self.size.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn rotation(max_size_mib: u32, max_files: u32, compress: bool) -> Rotation {
        Rotation {
            max_size_mib,
            max_files,
            max_age_days: 0,
            compress,
        }
    }

    /// Write enough bytes to force several rotations and assert: the live file
    /// keeps the stable `<name>.log` name, backups are pruned to `max_files`,
    /// and (when configured) backups are gzip-compressed.
    fn force_rotations(compress: bool) {
        let dir = tempfile::tempdir().expect("tempdir");
        // 1 MiB cap, keep 2 backups.
        let mut w =
            RollingWriter::new(dir.path(), "main", rotation(1, 2, compress)).expect("writer");

        // Each chunk ~0.6 MiB; writing 8 of them forces multiple rotations.
        let chunk = vec![b'x'; 600 * 1024];
        for _ in 0..8 {
            w.write_all(&chunk).expect("write");
            w.flush().expect("flush");
            // Distinct backup timestamps need at least millisecond separation.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        drop(w);

        // The live file keeps the stable name.
        let live = dir.path().join("main.log");
        assert!(live.exists(), "live file main.log must exist");

        // Backups are pruned to max_files (2).
        let suffix = if compress { ".log.gz" } else { ".log" };
        let mut backups: Vec<_> = fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("main-") && n.ends_with(suffix))
            .collect();
        backups.sort();
        assert_eq!(
            backups.len(),
            2,
            "backups must be pruned to max_files=2, got {backups:?}"
        );

        if compress {
            // A gzip backup round-trips back to the 'x' payload.
            let first = backups.first().expect("at least one backup");
            let path = dir.path().join(first);
            let bytes = fs::read(&path).expect("read gz");
            let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).expect("gunzip");
            assert!(!out.is_empty(), "decompressed backup must be non-empty");
            assert!(out.iter().all(|&b| b == b'x'), "payload preserved");
        }
    }

    #[test]
    fn rotates_and_prunes_to_max_files() {
        force_rotations(false);
    }

    #[test]
    fn rotates_compresses_and_prunes() {
        force_rotations(true);
    }

    #[test]
    fn appends_to_existing_live_file_without_rotation() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut w =
                RollingWriter::new(dir.path(), "main", rotation(8, 7, false)).expect("writer");
            w.write_all(b"first\n").expect("write");
        }
        // Re-open: should append, not truncate, and not rotate (well under cap).
        {
            let mut w =
                RollingWriter::new(dir.path(), "main", rotation(8, 7, false)).expect("writer");
            w.write_all(b"second\n").expect("write");
        }
        let contents = fs::read_to_string(dir.path().join("main.log")).expect("read");
        assert_eq!(contents, "first\nsecond\n");
        // No backups were produced.
        let backups = fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("main-"))
            .count();
        assert_eq!(backups, 0);
    }
}
