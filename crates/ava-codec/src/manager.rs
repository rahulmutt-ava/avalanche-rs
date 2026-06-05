// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Versioned codec [`Manager`] and the [`Codec`] trait.
//!
//! Port of Go's `codec/manager.go` + `codec/codec.go`. The [`Manager`] holds one
//! [`Codec`] per `u16` version and frames every top-level value with a **2-byte
//! big-endian version prefix**. On decode it enforces the mandatory
//! **trailing-byte check** ([`CodecError::ExtraSpace`]) — part of consensus
//! validation (`specs/03-core-primitives.md` §2.2, `specs/15` §6).

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::error::{CodecError, PackerError, Result};
use crate::packer::{Packer, SHORT_LEN};
use crate::{Deserializable, Serializable};

/// Width of the codec version prefix (`codec/manager.go`).
pub const VERSION_SIZE: usize = SHORT_LEN;
/// Default maximum decode size (256 KiB).
pub const DEFAULT_MAX_SIZE: usize = 256 * 1024;
/// Capacity hint for decode-side slice allocation (re-exported from the crate
/// root; see [`crate::INITIAL_SLICE_CAP`]).
pub const INITIAL_SLICE_CAP: usize = crate::INITIAL_SLICE_CAP;

/// A concrete codec: one type registry + tag set. Mirrors `codec.Codec`.
///
/// The linear-codec wire encoding lives in the value's [`Serializable`] /
/// [`Deserializable`] impls (derive-generated). A `Codec` adapts that streaming
/// API to a fallible one, translating the packer's sticky [`PackerError`] into a
/// [`CodecError`].
pub trait Codec: Send + Sync {
    /// Marshals `value` into `p`, returning the first sticky error if any.
    fn marshal_into(&self, value: &dyn Serializable, p: &mut Packer) -> Result<()>;

    /// Unmarshals from `p` into `dst`, returning the first sticky error if any.
    fn unmarshal_from(&self, p: &mut Packer, dst: &mut dyn Deserializable) -> Result<()>;

    /// The marshaled size of `value`, excluding the version prefix.
    fn size(&self, value: &dyn Serializable) -> Result<usize>;
}

/// Maps a low-level [`PackerError`] to its [`CodecError`] identity.
pub(crate) fn map_packer_error(err: PackerError) -> CodecError {
    match err {
        // A slice/map count or limited read overran its bound — the codec-level
        // identity is MaxSliceLenExceeded.
        PackerError::Oversized => CodecError::MaxSliceLenExceeded,
        other => CodecError::Packer(other),
    }
}

/// Versioned codec registry. Mirrors `codec.Manager`.
pub struct Manager {
    max_size: usize,
    codecs: RwLock<HashMap<u16, Arc<dyn Codec>>>,
}

impl Manager {
    /// Creates a manager with the given maximum decode size.
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            codecs: RwLock::new(HashMap::new()),
        }
    }

    /// Creates a manager with [`DEFAULT_MAX_SIZE`].
    #[must_use]
    pub fn with_default_max_size() -> Self {
        Self::new(DEFAULT_MAX_SIZE)
    }

    /// The configured maximum decode size.
    #[must_use]
    pub fn max_size(&self) -> usize {
        self.max_size
    }

    /// Registers `codec` under `version`. Errors with
    /// [`CodecError::DuplicatedVersion`] if the version is already present.
    pub fn register(&self, version: u16, codec: Arc<dyn Codec>) -> Result<()> {
        let mut codecs = self.codecs.write();
        if codecs.contains_key(&version) {
            return Err(CodecError::DuplicatedVersion);
        }
        codecs.insert(version, codec);
        Ok(())
    }

    /// Marshals `value` with the `version` codec, returning the full buffer
    /// including the 2-byte version prefix.
    pub fn marshal(&self, version: u16, value: &dyn Serializable) -> Result<Vec<u8>> {
        let codec = self.codec(version)?;
        let size = codec
            .size(value)?
            .checked_add(VERSION_SIZE)
            .ok_or(CodecError::MaxSliceLenExceeded)?;
        let mut p = Packer::with_max_size(self.max_size.max(size));
        p.pack_u16(version);
        if p.error().is_some() {
            return Err(CodecError::CantPackVersion);
        }
        codec.marshal_into(value, &mut p)?;
        if let Some(err) = p.error() {
            return Err(map_packer_error(err));
        }
        Ok(p.into_bytes())
    }

    /// Unmarshals `src` into `dst`, returning the decoded codec version.
    ///
    /// Rejects `src.len() > max_size` ([`CodecError::UnmarshalTooBig`]), an
    /// unknown version ([`CodecError::UnknownVersion`]), and — after a clean
    /// decode — any trailing bytes ([`CodecError::ExtraSpace`]).
    pub fn unmarshal(&self, src: &[u8], dst: &mut dyn Deserializable) -> Result<u16> {
        if src.len() > self.max_size {
            return Err(CodecError::UnmarshalTooBig);
        }
        let mut p = Packer::new_read(src);
        let version = p.unpack_u16();
        if p.error().is_some() {
            return Err(CodecError::CantUnpackVersion);
        }
        let codec = self.codec(version)?;
        codec.unmarshal_from(&mut p, dst)?;
        if let Some(err) = p.error() {
            return Err(map_packer_error(err));
        }
        // Mandatory trailing-byte check (consensus validation).
        if p.offset() != src.len() {
            return Err(CodecError::ExtraSpace);
        }
        Ok(version)
    }

    /// The marshaled size of `value` under `version`, **including** the version
    /// prefix.
    pub fn size(&self, version: u16, value: &dyn Serializable) -> Result<usize> {
        let codec = self.codec(version)?;
        codec
            .size(value)?
            .checked_add(VERSION_SIZE)
            .ok_or(CodecError::MaxSliceLenExceeded)
    }

    /// Looks up the codec for `version`.
    fn codec(&self, version: u16) -> Result<Arc<dyn Codec>> {
        self.codecs
            .read()
            .get(&version)
            .cloned()
            .ok_or(CodecError::UnknownVersion)
    }
}

impl Default for Manager {
    fn default() -> Self {
        Self::with_default_max_size()
    }
}
