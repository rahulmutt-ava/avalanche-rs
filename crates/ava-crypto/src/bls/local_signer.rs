// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! File-backed `LocalSigner` (zeroized, `0o400` on disk).
//!
//! Port of Go `utils/crypto/bls/signers/local/local.go` (`New`, `FromBytes`,
//! `FromFile`, `ToFile`, `FromFileOrPersistNew`). The on-disk format is the raw
//! 32-byte big-endian `SecretKey` serialization (`blst`), **not** PEM, so a
//! Go-written `signer.key` opens identically and vice-versa. The secret scalar
//! is held in `zeroize::Zeroizing` and the generation IKM is zeroized after use.
//! On Unix the key file is written `0o400` and its parent directory `0o700`.
//! Owning spec: `specs/25-key-management-and-signing.md` §3.2, §6.

use std::path::Path;

use ring::rand::{SecureRandom, SystemRandom};
use zeroize::Zeroizing;

use super::keys::{PublicKey, SECRET_KEY_LEN, SecretKey};
use super::sign::Signature;
use super::signer::Signer;
use crate::error::{Error, Result};

/// An in-process BLS signer backed by a locally held secret key.
///
/// The 32-byte big-endian secret scalar is kept in [`Zeroizing`] and wiped on
/// drop; the public key is derived once and cached.
pub struct LocalSigner {
    /// 32-byte big-endian secret scalar; zeroized on drop.
    sk: Zeroizing<[u8; SECRET_KEY_LEN]>,
    /// Cached public key (G1).
    pk: PublicKey,
}

impl LocalSigner {
    /// `localsigner.New` — generate a fresh key from 32 bytes of CSPRNG IKM.
    ///
    /// The IKM is zeroized immediately after key generation.
    ///
    /// # Errors
    /// [`Error::FailedSecretKeyDeserialize`] if key generation rejects the IKM;
    /// [`Error::Io`] if the system RNG fails.
    pub fn generate() -> Result<Self> {
        let mut ikm = Zeroizing::new([0u8; SECRET_KEY_LEN]);
        SystemRandom::new()
            .fill(ikm.as_mut())
            .map_err(|_| Error::Io("system RNG failed".into()))?;
        let sk = SecretKey::new(&ikm)?;
        Ok(Self::from_secret_key(sk))
    }

    /// `localsigner.FromBytes` — big-endian deserialize of the 32-byte scalar.
    ///
    /// # Errors
    /// [`Error::FailedSecretKeyDeserialize`] if the bytes are not a valid key.
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        let sk = SecretKey::from_bytes(b)?;
        Ok(Self::from_secret_key(sk))
    }

    /// `localsigner.FromFile` — read the 32-byte file then [`Self::from_bytes`].
    ///
    /// # Errors
    /// [`Error::Io`] on a read failure; [`Error::FailedSecretKeyDeserialize`] if
    /// the contents are not a valid key.
    pub fn from_file(path: &Path) -> Result<Self> {
        let bytes = Zeroizing::new(std::fs::read(path).map_err(|e| Error::Io(e.to_string()))?);
        Self::from_bytes(&bytes)
    }

    /// `localsigner.ToFile` — write the raw 32-byte key; create the parent dir
    /// `0o700` and set the file `0o400` (Unix).
    ///
    /// # Errors
    /// [`Error::Io`] on any filesystem failure.
    pub fn to_file(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io(e.to_string()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                    .map_err(|e| Error::Io(e.to_string()))?;
            }
        }
        std::fs::write(path, self.sk.as_ref()).map_err(|e| Error::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400))
                .map_err(|e| Error::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// `localsigner.FromFileOrPersistNew` — load the key if `path` exists, else
    /// generate a fresh one and persist it.
    ///
    /// # Errors
    /// [`Error::Io`] on a filesystem failure;
    /// [`Error::FailedSecretKeyDeserialize`] if an existing file is malformed.
    pub fn from_file_or_persist_new(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::from_file(path);
        }
        let signer = Self::generate()?;
        signer.to_file(path)?;
        Ok(signer)
    }

    /// Cache the public key and capture the big-endian secret bytes.
    fn from_secret_key(sk: SecretKey) -> Self {
        let pk = sk.public_key();
        let bytes = Zeroizing::new(sk.to_bytes());
        Self { sk: bytes, pk }
    }

    /// Reconstruct the `blst` secret key for a signing operation. The returned
    /// key zeroizes on drop (via `blst`).
    fn secret_key(&self) -> Result<SecretKey> {
        SecretKey::from_bytes(self.sk.as_ref())
    }
}

impl Signer for LocalSigner {
    fn public_key(&self) -> &PublicKey {
        &self.pk
    }

    fn sign(&self, msg: &[u8]) -> Result<Signature> {
        Ok(self.secret_key()?.sign(msg))
    }

    fn sign_proof_of_possession(&self, msg: &[u8]) -> Result<Signature> {
        Ok(self.secret_key()?.sign_pop(msg))
    }
}
