// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The persisted [`Container`] and its linear codec (Go `indexer/container.go`
//! + `indexer/codec.go`).
//!
//! A container is "something that gets accepted" — a block, transaction, or
//! vertex. The on-disk value stored under `index → container` is the
//! linear-codec marshaling of [`Container`] framed by the 2-byte codec version
//! (`CodecVersion = 0`), byte-identical to Go:
//!
//! ```text
//! u16 version (0) ‖ id (32 raw bytes) ‖ u32 len ‖ bytes ‖ i64 timestamp (BE)
//! ```

use std::sync::{Arc, OnceLock};

use ava_codec::linearcodec::LinearCodec;
use ava_codec::manager::Manager;
use ava_codec::packer::Packer;
use ava_codec::{Deserializable, Serializable};
use ava_types::id::{ID_LEN, Id};

use crate::error::{Error, Result};

/// The codec version every container is marshaled under (Go `CodecVersion`).
pub const CODEC_VERSION: u16 = 0;

/// Something that got accepted (Go `indexer.Container`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Container {
    /// ID of this container.
    pub id: Id,
    /// Byte representation of this container.
    pub bytes: Vec<u8>,
    /// Unix time, in nanoseconds, at which this container was accepted by
    /// this node.
    pub timestamp: i64,
}

impl Serializable for Container {
    fn marshal_into(&self, p: &mut Packer) {
        self.id.marshal_into(p);
        // `Vec<u8>` marshals as Go `[]byte`: u32 length prefix + raw bytes.
        self.bytes.marshal_into(p);
        // Go int64 is 8 big-endian bytes; the bit pattern is sign-preserving.
        p.pack_u64(self.timestamp.cast_unsigned());
    }

    fn size(&self) -> usize {
        // id + u32 length prefix + payload + i64 timestamp.
        ID_LEN
            .saturating_add(4)
            .saturating_add(self.bytes.len())
            .saturating_add(8)
    }
}

impl Deserializable for Container {
    fn unmarshal_from(&mut self, p: &mut Packer) {
        self.id.unmarshal_from(p);
        self.bytes.unmarshal_from(p);
        self.timestamp = p.unpack_u64().cast_signed();
    }
}

/// The shared codec manager (Go `indexer/codec.go::Codec`, version 0,
/// unbounded decode size — Go uses `math.MaxInt`).
fn codec() -> &'static Manager {
    static CODEC: OnceLock<Manager> = OnceLock::new();
    CODEC.get_or_init(|| {
        let m = Manager::new(usize::MAX);
        // A freshly constructed manager cannot already hold version 0.
        if m.register(CODEC_VERSION, Arc::new(LinearCodec::new()))
            .is_err()
        {
            unreachable!("fresh codec manager rejected version 0");
        }
        m
    })
}

impl Container {
    /// Marshals this container under [`CODEC_VERSION`]
    /// (Go `Codec.Marshal(CodecVersion, container)`).
    ///
    /// # Errors
    /// Returns [`Error::SerializeContainer`] if the codec rejects the value
    /// (e.g. `bytes` longer than `i32::MAX`).
    pub fn marshal(&self) -> Result<Vec<u8>> {
        codec()
            .marshal(CODEC_VERSION, self)
            .map_err(|source| Error::SerializeContainer {
                id: self.id,
                source,
            })
    }

    /// Unmarshals a container from its versioned wire bytes
    /// (Go `Codec.Unmarshal(bytes, &container)`).
    ///
    /// # Errors
    /// Returns [`Error::UnmarshalContainer`] on a malformed value.
    pub fn unmarshal(src: &[u8]) -> Result<Self> {
        let mut container = Self::default();
        codec()
            .unmarshal(src, &mut container)
            .map_err(Error::UnmarshalContainer)?;
        Ok(container)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    /// The wire layout is byte-identical to Go's linear codec: 2-byte version,
    /// 32-byte id, u32 length prefix + payload, 8-byte big-endian timestamp.
    #[test]
    fn container_codec_layout() {
        let container = Container {
            id: Id::from([0xAB; 32]),
            bytes: vec![0x01, 0x02, 0x03],
            timestamp: 0x0102_0304_0506_0708,
        };
        let got = container.marshal().expect("Container::marshal()");

        let mut want = vec![0x00, 0x00]; // codec version 0
        want.extend_from_slice(&[0xAB; 32]);
        want.extend_from_slice(&[0x00, 0x00, 0x00, 0x03]);
        want.extend_from_slice(&[0x01, 0x02, 0x03]);
        want.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
        assert_eq!(want, got, "Container codec layout");

        let back = Container::unmarshal(&got).expect("Container::unmarshal()");
        assert_eq!(container, back, "Container codec round-trip");
    }

    /// Trailing bytes are rejected (the manager's mandatory ExtraSpace check).
    #[test]
    fn container_unmarshal_rejects_trailing_bytes() {
        let mut bytes = Container::default()
            .marshal()
            .expect("Container::marshal()");
        bytes.push(0xFF);
        Container::unmarshal(&bytes).expect_err("Container::unmarshal() with trailing byte");
    }
}
