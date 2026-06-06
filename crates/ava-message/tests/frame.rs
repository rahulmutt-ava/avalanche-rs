// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.3 — 4-byte big-endian length-prefix helpers + the 2 MiB cap, byte-exact
//! with `network/peer/msg_length.go` (specs/05 §1.1/§2.3, 15 §4.2).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    unused_crate_dependencies
)]

use assert_matches::assert_matches;
use bytes::BytesMut;

use ava_message::Error;
use ava_message::frame::{MAX_MESSAGE_SIZE, read_msg_len, write_msg_len};

#[test]
fn max_message_size_is_2_mib() {
    assert_eq!(MAX_MESSAGE_SIZE, 2 * 1024 * 1024);
}

#[test]
fn read_msg_len_be_and_cap() {
    assert_eq!(read_msg_len([0, 0, 0, 4], MAX_MESSAGE_SIZE).unwrap(), 4);
    assert_eq!(read_msg_len([0, 0, 0, 0], MAX_MESSAGE_SIZE).unwrap(), 0);
    assert_eq!(
        read_msg_len([0, 0x20, 0, 0], MAX_MESSAGE_SIZE).unwrap(),
        MAX_MESSAGE_SIZE
    );

    // 0x00200001 == 2 MiB + 1 -> over the cap.
    assert_matches!(
        read_msg_len([0, 0x20, 0, 1], MAX_MESSAGE_SIZE),
        Err(Error::MaxMessageLengthExceeded { len, max })
            if len == MAX_MESSAGE_SIZE + 1 && max == MAX_MESSAGE_SIZE
    );
}

#[test]
fn write_msg_len_be_and_cap() {
    let mut buf = BytesMut::new();
    write_msg_len(&mut buf, 4).unwrap();
    assert_eq!(&buf[..], &[0, 0, 0, 4]);

    let mut buf = BytesMut::new();
    write_msg_len(&mut buf, MAX_MESSAGE_SIZE).unwrap();
    assert_eq!(&buf[..], &[0, 0x20, 0, 0]);

    let mut buf = BytesMut::new();
    assert_matches!(
        write_msg_len(&mut buf, MAX_MESSAGE_SIZE + 1),
        Err(Error::MaxMessageLengthExceeded { .. })
    );
}
