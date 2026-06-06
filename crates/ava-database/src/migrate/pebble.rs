// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Pebble reader for the import tool (04 §11.3) — via a Go export sidecar.
//!
//! # In-place Pebble open is NOT supported
//!
//! A Go node written on v1.10.15+ stores its base DB as **Pebble** under
//! `pebble/`. There is **no production-grade pure-Rust Pebble reader**, so the
//! Rust node cannot open a Pebble dir in place (04 §7, §11.3). The only
//! correctness-guaranteed path is a small **Go export sidecar**
//! (`avalanchego-db-export`, ~80 LOC) that links the real `database/pebbledb`
//! and streams every pair as a length-prefixed framing on stdout. Because the
//! sidecar reuses Go's own Pebble open path, it reads any dir a Go node wrote.
//!
//! [`PebbleSidecarSource`] spawns that sidecar and parses its stream. The
//! framing parser ([`parse_frame`]) is fully implemented and unit-tested; the
//! sidecar **spawn** is a documented stub (the sidecar binary ships with the CLI
//! in M12). See `crates/ava-database/docs/migration.md`.

use std::io::Read;

use crate::migrate::GoDbSource;

/// The conventional on-disk subdirectory name of a Pebble base DB written by
/// avalanchego (`pebble/`). Used by the CLI's backend auto-detection (04 §11.3);
/// `--db-type pebble` overrides.
pub const PEBBLE_DIR_NAME: &str = "pebble";

/// The default name of the Go export sidecar binary (04 §11.3). The CLI resolves
/// it on `PATH` (or via an explicit `--sidecar` path) in M12.
pub const SIDECAR_BIN: &str = "avalanchego-db-export";

/// A Pebble reader that spawns the [`SIDECAR_BIN`] Go sidecar and parses its
/// length-prefixed `(key, value)` stream (04 §11.3).
///
/// # Stream framing
///
/// Each pair is framed as:
///
/// ```text
/// u32 key_len (big-endian) ‖ key ‖ u32 value_len (big-endian) ‖ value
/// ```
///
/// The stream ends at clean EOF. Bytes are **never transformed** — the sidecar
/// emits exactly what Pebble stored, in lexicographic key order, so the framing
/// reader is a pure length-prefixed decode.
///
/// # Status — spawn is a documented stub
///
/// [`iter_all`](GoDbSource::iter_all) currently returns an explanatory error
/// rather than launching a process, because the sidecar binary is built and
/// shipped with the CLI assembly in M12. The framing parser is real and tested
/// (see [`parse_stream`]); when M12 lands, `iter_all` swaps the stub for a
/// `std::process::Command` spawn whose stdout is fed straight into
/// [`parse_stream`].
pub struct PebbleSidecarSource {
    dir: std::path::PathBuf,
    sidecar: std::path::PathBuf,
}

impl PebbleSidecarSource {
    /// Targets the Pebble directory `dir`, resolving the sidecar from
    /// [`SIDECAR_BIN`] on `PATH`.
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            sidecar: std::path::PathBuf::from(SIDECAR_BIN),
        }
    }

    /// Targets `dir` using an explicit `sidecar` binary path (the CLI's
    /// `--sidecar` override).
    pub fn with_sidecar(
        dir: impl Into<std::path::PathBuf>,
        sidecar: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            dir: dir.into(),
            sidecar: sidecar.into(),
        }
    }

    /// The Pebble directory this reader targets.
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// The resolved sidecar binary path.
    pub fn sidecar(&self) -> &std::path::Path {
        &self.sidecar
    }
}

impl GoDbSource for PebbleSidecarSource {
    fn iter_all(&self) -> anyhow::Result<Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)>>> {
        anyhow::bail!(
            "PebbleSidecarSource: spawning the `{}` Go export sidecar over `{}` \
             is wired in M12 (the sidecar binary ships with the CLI). In-place \
             Pebble open is NOT supported (04 §7/§11.3). The length-prefixed \
             frame parser is implemented and tested. See \
             crates/ava-database/docs/migration.md.",
            self.sidecar.display(),
            self.dir.display(),
        )
    }
}

/// Reads one length-prefixed `(key, value)` frame from `r` (04 §11.3 framing).
///
/// Returns `Ok(None)` at clean EOF (no bytes left to read a frame), `Ok(Some(..))`
/// for a complete frame, and an error for a truncated/oversized frame.
///
/// # Errors
///
/// Returns an error if the stream ends mid-frame or a declared length cannot be
/// read.
pub fn parse_frame<R: Read>(r: &mut R) -> anyhow::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut len_buf = [0u8; 4];
    // First field: peek whether we're at clean EOF.
    match read_exact_or_eof(r, &mut len_buf)? {
        ReadOutcome::Eof => return Ok(None),
        ReadOutcome::Full => {}
    }
    let key_len = u32::from_be_bytes(len_buf) as usize;
    let mut key = vec![0u8; key_len];
    r.read_exact(&mut key)
        .map_err(|e| anyhow::anyhow!("truncated frame: reading key ({key_len} bytes): {e}"))?;

    r.read_exact(&mut len_buf)
        .map_err(|e| anyhow::anyhow!("truncated frame: reading value length: {e}"))?;
    let value_len = u32::from_be_bytes(len_buf) as usize;
    let mut value = vec![0u8; value_len];
    r.read_exact(&mut value)
        .map_err(|e| anyhow::anyhow!("truncated frame: reading value ({value_len} bytes): {e}"))?;

    Ok(Some((key, value)))
}

/// Parses an entire sidecar stream into an ordered vector of pairs.
///
/// # Errors
///
/// Propagates any per-frame parse error from [`parse_frame`].
pub fn parse_stream<R: Read>(mut r: R) -> anyhow::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut out = Vec::new();
    while let Some(pair) = parse_frame(&mut r)? {
        out.push(pair);
    }
    Ok(out)
}

/// Outcome of trying to read a fixed-size header at a frame boundary.
enum ReadOutcome {
    /// All requested bytes were read (a frame follows).
    Full,
    /// Zero bytes were available (clean end of stream).
    Eof,
}

/// Reads exactly `buf.len()` bytes, distinguishing clean EOF (zero bytes read at
/// the very start) from a truncated read (partial bytes — an error).
fn read_exact_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> anyhow::Result<ReadOutcome> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let slice = buf
            .get_mut(filled..)
            .ok_or_else(|| anyhow::anyhow!("frame header slice out of range"))?;
        match r.read(slice) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(ReadOutcome::Eof);
                }
                anyhow::bail!("truncated frame header: {filled}/{} bytes", buf.len());
            }
            Ok(n) => filled = filled.saturating_add(n),
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(anyhow::anyhow!("frame header read error: {e}")),
        }
    }
    Ok(ReadOutcome::Full)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes pairs into the sidecar framing so the parser can round-trip them.
    fn encode(pairs: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (k, v) in pairs {
            buf.extend_from_slice(&(k.len() as u32).to_be_bytes());
            buf.extend_from_slice(k);
            buf.extend_from_slice(&(v.len() as u32).to_be_bytes());
            buf.extend_from_slice(v);
        }
        buf
    }

    #[test]
    fn frame_roundtrip_preserves_bytes() {
        let pairs = vec![
            (b"singleton".to_vec(), b"last-accepted".to_vec()),
            (Vec::new(), b"empty-key".to_vec()),
            (b"k".to_vec(), Vec::new()),
            (vec![0xff, 0x00, 0xde], vec![0xad, 0xbe, 0xef]),
        ];
        let encoded = encode(&pairs);
        let decoded = parse_stream(&encoded[..]).expect("parse");
        assert_eq!(decoded, pairs);
    }

    #[test]
    fn empty_stream_is_clean_eof() {
        let decoded = parse_stream(&[][..]).expect("parse");
        assert!(decoded.is_empty());
    }

    #[test]
    fn truncated_frame_errors() {
        // A key-length header promising 4 bytes but with only 2 following.
        let mut buf = (4u32).to_be_bytes().to_vec();
        buf.extend_from_slice(b"ab");
        let err = parse_stream(&buf[..]).expect_err("must error");
        assert!(err.to_string().contains("truncated frame"));
    }

    #[test]
    fn sidecar_spawn_is_documented_stub() {
        let src = PebbleSidecarSource::new("/data/pebble");
        assert_eq!(src.dir().to_string_lossy(), "/data/pebble");
        assert_eq!(src.sidecar().to_string_lossy(), SIDECAR_BIN);
        let err = src.iter_all().err().expect("spawn is stubbed").to_string();
        assert!(err.contains("wired in M12"));
    }
}
