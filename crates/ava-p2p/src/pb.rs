// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Generated `proto/sdk` messages (Go `proto/pb/sdk`).

/// `sdk` package messages.
pub mod sdk {
    #![allow(missing_docs, clippy::pedantic)]
    include!(concat!(env!("OUT_DIR"), "/sdk.rs"));
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::sdk;

    /// Proto3 wire bytes computed by hand: field 2 (salt, bytes) = tag 0x12,
    /// field 3 (filter, bytes) = tag 0x1a.
    #[test]
    fn pull_gossip_request_wire_bytes_pinned() {
        let req = sdk::PullGossipRequest {
            salt: bytes::Bytes::from_static(&[0xAA, 0xBB]),
            filter: bytes::Bytes::from_static(&[0x01]),
        };
        let enc = req.encode_to_vec();
        assert_eq!(enc, vec![0x12, 0x02, 0xAA, 0xBB, 0x1A, 0x01, 0x01]);
        let dec = sdk::PullGossipRequest::decode(enc.as_slice()).unwrap();
        assert_eq!(dec, req);
    }
}
