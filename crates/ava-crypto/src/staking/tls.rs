// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Staking cert + key generation (`rcgen`).
//!
//! Port of Go `staking.NewCertAndKeyBytes` (`staking/tls.go`): a self-signed
//! X.509 certificate with the exact Go template — ECDSA P-256, `SerialNumber=0`,
//! `NotBefore` = the Go `time.Date(2000, time.January, 0, …)` instant
//! (== `1999-12-31T00:00:00Z`), `NotAfter = now + 100 years`,
//! `KeyUsage = DigitalSignature`, `BasicConstraintsValid = true` (CA = false),
//! and no SAN / empty subject. Output is PEM: a `CERTIFICATE` block plus a
//! PKCS#8 `PRIVATE KEY` block. On Unix the files are written `0o400` and their
//! parent directory `0o700` (`perms.ReadOnly` / `perms.ReadWriteExecute`).
//! Owning spec: `specs/03-core-primitives.md` §3.6, `specs/25` §2.1.

use std::path::Path;

use rcgen::{CertificateParams, IsCa, KeyPair, KeyUsagePurpose, SerialNumber};
use time::{Duration, OffsetDateTime};

use crate::error::{Error, Result};

/// `NotBefore` for every staking cert: `1999-12-31T00:00:00Z`.
///
/// Go writes `time.Date(2000, time.January, 0, 0, 0, 0, 0, time.UTC)`; the
/// zeroth day of January 2000 normalizes to Dec 31 1999. Unix timestamp
/// `946_598_400` = 2000-01-01T00:00:00Z, so Dec 31 1999 = that minus one day.
const NOT_BEFORE_UNIX: i64 = 946_598_400 - 86_400;

/// Validity span: 100 years from `now`, matching Go (`now.AddDate(100, 0, 0)`).
/// `time::Duration` has no calendar arithmetic, so we approximate 100 Gregorian
/// years as 36525 days (== 100 * 365.25); the exact value is not consensus-
/// relevant (cert content is not a consensus constant — only NodeID derivation
/// is). See `specs/03` §3.6.
const HUNDRED_YEARS_DAYS: i64 = 36_525;

/// `staking.NewCertAndKeyBytes` — generate a fresh self-signed staking cert and
/// its private key, returned as PEM (`CERTIFICATE`, PKCS#8 `PRIVATE KEY`).
///
/// # Errors
/// [`Error::CertificateGenerate`] if key generation or self-signing fails.
pub fn new_cert_and_key_bytes() -> Result<(String, String)> {
    let key_pair = KeyPair::generate().map_err(|e| Error::CertificateGenerate(e.to_string()))?;

    let not_before = OffsetDateTime::from_unix_timestamp(NOT_BEFORE_UNIX)
        .map_err(|e| Error::CertificateGenerate(e.to_string()))?;
    let not_after = OffsetDateTime::now_utc()
        .checked_add(Duration::days(HUNDRED_YEARS_DAYS))
        .ok_or_else(|| Error::CertificateGenerate("not_after overflow".into()))?;

    let mut params = CertificateParams::default();
    params.not_before = not_before;
    params.not_after = not_after;
    // Go uses big.NewInt(0); a single zero octet is DER INTEGER 0.
    params.serial_number = Some(SerialNumber::from_slice(&[0]));
    // No SAN, empty subject — Go sets neither.
    params.subject_alt_names = Vec::new();
    // BasicConstraintsValid = true with IsCA = false.
    params.is_ca = IsCa::ExplicitNoCa;
    // KeyUsage = DigitalSignature only.
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| Error::CertificateGenerate(e.to_string()))?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}

/// Write the staking cert + key PEM to `cert_path` / `key_path`, replicating Go
/// `perms`: the parent directories are created `0o700` and both files are made
/// read-only (`0o400`) after writing (on Unix).
///
/// # Errors
/// [`Error::Io`] on any filesystem failure.
pub fn write_cert_and_key(
    cert_path: &Path,
    key_path: &Path,
    cert_pem: &str,
    key_pem: &str,
) -> Result<()> {
    write_readonly(cert_path, cert_pem.as_bytes())?;
    write_readonly(key_path, key_pem.as_bytes())?;
    Ok(())
}

/// Create the parent directory `0o700` (Unix), write `bytes` to `path`, then set
/// the file mode to `0o400` (Unix). On non-Unix platforms perms are skipped.
fn write_readonly(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_dir_0700(parent)?;
    }
    std::fs::write(path, bytes).map_err(|e| Error::Io(e.to_string()))?;
    set_mode_0400(path)?;
    Ok(())
}

/// Create `dir` (and ancestors) with mode `0o700` on Unix.
fn create_dir_0700(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir, perms).map_err(|e| Error::Io(e.to_string()))?;
    }
    Ok(())
}

/// Set `path`'s mode to `0o400` (read-only owner) on Unix.
fn set_mode_0400(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o400);
        std::fs::set_permissions(path, perms).map_err(|e| Error::Io(e.to_string()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
