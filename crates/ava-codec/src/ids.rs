// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Codec impls for the `ava-types` fixed-byte identifiers.
//!
//! `ava-types` cannot depend on `ava-codec` (it would close the dependency
//! cycle — `ava-codec` depends on `ava-types`; see `specs/03` §0). The
//! `Serializable`/`Deserializable` impls therefore live **here**, the layer
//! directly above `ava-types`, so every downstream crate (P-Chain / X-Chain txs,
//! …) can embed an [`Id`] / [`ShortId`] / [`NodeId`] as a `#[codec]` field
//! without re-deriving the orphan-blocked impl.
//!
//! Each identifier is a fixed-width byte array on the wire (no length prefix),
//! identical to Go's `[N]byte` handling in `reflectcodec/type_codec.go`.

use ava_types::id::{ID_LEN, Id};
use ava_types::node_id::{NODE_ID_LEN, NodeId};
use ava_types::short_id::{SHORT_ID_LEN, ShortId};

use crate::packer::Packer;
use crate::{Deserializable, Serializable};

macro_rules! impl_fixed_id_codec {
    ($t:ty, $len:expr) => {
        impl Serializable for $t {
            fn marshal_into(&self, p: &mut Packer) {
                p.pack_fixed_bytes(self.as_bytes());
            }

            fn size(&self) -> usize {
                $len
            }
        }

        impl Deserializable for $t {
            fn unmarshal_from(&mut self, p: &mut Packer) {
                let raw = p.unpack_fixed_bytes($len);
                if p.errored() {
                    return;
                }
                if let Ok(arr) = <[u8; $len]>::try_from(raw.as_slice()) {
                    *self = <$t>::from(arr);
                }
            }
        }
    };
}

impl_fixed_id_codec!(Id, ID_LEN);
impl_fixed_id_codec!(ShortId, SHORT_ID_LEN);
impl_fixed_id_codec!(NodeId, NODE_ID_LEN);

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: T, expected_len: usize)
    where
        T: Serializable + Deserializable + Default + PartialEq + core::fmt::Debug,
    {
        assert_eq!(value.size(), expected_len);
        let mut p = Packer::with_max_size(64);
        value.marshal_into(&mut p);
        let bytes = p.into_bytes();
        assert_eq!(bytes.len(), expected_len);

        let mut decoded = T::default();
        let mut rp = Packer::new_read(&bytes);
        decoded.unmarshal_from(&mut rp);
        assert!(!rp.errored());
        assert_eq!(decoded, value);
    }

    #[test]
    fn id_roundtrip_no_prefix() {
        roundtrip(Id::from([7u8; ID_LEN]), ID_LEN);
    }

    #[test]
    fn short_id_roundtrip_no_prefix() {
        roundtrip(ShortId::from([3u8; SHORT_ID_LEN]), SHORT_ID_LEN);
    }

    #[test]
    fn node_id_roundtrip_no_prefix() {
        roundtrip(NodeId::from([9u8; NODE_ID_LEN]), NODE_ID_LEN);
    }
}
