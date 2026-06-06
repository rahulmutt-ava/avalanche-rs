// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! 4-byte big-endian length-prefix framing helpers — byte-exact with
//! `network/peer/msg_length.go` (specs/05 §1.1/§2.3, 15 §4.2).
//!
//! The only bytes on the socket are `len_be_u32 || proto_bytes`. A length
//! prefix `> MAX_MESSAGE_SIZE` (2 MiB) is a protocol error and the connection is
//! dropped — these helpers enforce that cap on both the read and write paths.

use bytes::{BufMut, BytesMut};

use crate::error::{Error, Result};

/// Maximum p2p message payload size (`DefaultMaxMessageSize = 2 * units.MiB`).
/// A length prefix exceeding this is a protocol error.
pub const MAX_MESSAGE_SIZE: u32 = 2 * 1024 * 1024;

/// Appends the 4-byte big-endian length prefix `len` to `buf`.
///
/// Mirrors Go `writeMsgLen`: rejects `len > max` before writing anything.
///
/// # Errors
/// Returns [`Error::MaxMessageLengthExceeded`] if `len > max`.
pub fn write_msg_len(buf: &mut BytesMut, len: u32) -> Result<()> {
    write_msg_len_capped(buf, len, MAX_MESSAGE_SIZE)
}

/// Like [`write_msg_len`] but with an explicit cap (mirrors Go's `maxMsgLen`
/// parameter).
///
/// # Errors
/// Returns [`Error::MaxMessageLengthExceeded`] if `len > max`.
pub fn write_msg_len_capped(buf: &mut BytesMut, len: u32, max: u32) -> Result<()> {
    if len > max {
        return Err(Error::MaxMessageLengthExceeded { len, max });
    }
    buf.put_u32(len); // big-endian
    Ok(())
}

/// Parses the 4-byte big-endian length prefix `b`, enforcing the `max` cap.
///
/// Mirrors Go `readMsgLen`. The fixed `[u8; 4]` argument makes the
/// "exactly 4 bytes" invariant a compile-time guarantee (Go checks it at
/// runtime), so the `InvalidMessageLength` path is unreachable here by
/// construction.
///
/// # Errors
/// Returns [`Error::MaxMessageLengthExceeded`] if the decoded length `> max`.
pub fn read_msg_len(b: [u8; 4], max: u32) -> Result<u32> {
    let len = u32::from_be_bytes(b);
    if len > max {
        return Err(Error::MaxMessageLengthExceeded { len, max });
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn write_then_hex_roundtrips() {
        let mut buf = BytesMut::new();
        write_msg_len(&mut buf, 0x0001_0203).unwrap();
        // hex of the big-endian prefix.
        assert_eq!(hex::encode(buf.as_ref()), "00010203");
    }

    proptest! {
        // write_msg_len then read_msg_len is the identity for any in-range length.
        #[test]
        fn write_read_identity(len in 0u32..=MAX_MESSAGE_SIZE) {
            let mut buf = BytesMut::new();
            write_msg_len(&mut buf, len).unwrap();
            let prefix: [u8; 4] = buf.as_ref().try_into().unwrap();
            prop_assert_eq!(read_msg_len(prefix, MAX_MESSAGE_SIZE).unwrap(), len);
        }
    }
}
