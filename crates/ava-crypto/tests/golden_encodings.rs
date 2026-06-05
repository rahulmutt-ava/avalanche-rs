// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! EXIT-GATE golden test `golden::cb58_addr_bech32` (M0.17).
//!
//! Mirrors avalanchego `utils/formatting/encoding_test.go` and
//! `utils/formatting/address/address_test.go`. Vectors are Go-generated.

use ava_crypto::address;
use ava_crypto::formatting::{decode, encode, Encoding};

#[derive(serde::Deserialize)]
struct Bech32Case {
    alias: String,
    hrp: String,
    payload_hex: String,
    bech32: String,
    formatted: String,
}

#[derive(serde::Deserialize)]
struct HexCase {
    payload_hex: String,
    hex: String,
    hex_nc: String,
    hex_c: String,
}

#[derive(serde::Deserialize)]
struct Vectors {
    bech32: Vec<Bech32Case>,
    hex: Vec<HexCase>,
}

fn vectors() -> Vectors {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/crypto/encodings.json"
    ))
    .expect("read encodings.json");
    serde_json::from_str(&raw).expect("parse encodings.json")
}

mod golden {
    use super::*;

    #[test]
    fn cb58_addr_bech32() {
        let v = vectors();

        // --- hex / hex-checksum / hex-no-checksum ---
        for c in &v.hex {
            let payload = hex::decode(&c.payload_hex).expect("decode payload");

            assert_eq!(encode(Encoding::Hex, &payload).expect("hex"), c.hex);
            assert_eq!(encode(Encoding::HexC, &payload).expect("hexc"), c.hex_c);
            assert_eq!(encode(Encoding::HexNc, &payload).expect("hexnc"), c.hex_nc);

            // Decode round-trips for each (default Hex verifies + strips checksum).
            assert_eq!(decode(Encoding::Hex, &c.hex).expect("dec hex"), payload);
            assert_eq!(decode(Encoding::HexC, &c.hex_c).expect("dec hexc"), payload);
            assert_eq!(
                decode(Encoding::HexNc, &c.hex_nc).expect("dec hexnc"),
                payload
            );
        }

        // missing 0x prefix -> error
        assert!(decode(Encoding::Hex, "deadbeefaa813953").is_err());
        // bad checksum -> error
        assert!(decode(Encoding::Hex, "0xdeadbeef00000000").is_err());
        // Json encoding unsupported on this path
        assert!(encode(Encoding::Json, &[1, 2, 3]).is_err());
        assert!(decode(Encoding::Json, "0x").is_err());

        // --- bech32 chain-prefixed addresses ---
        for c in &v.bech32 {
            let payload = hex::decode(&c.payload_hex).expect("decode payload");

            // raw bech32 (hrp + payload)
            let raw = address::format_bech32(&c.hrp, &payload).expect("format_bech32");
            assert_eq!(raw, c.bech32);
            let (hrp_back, payload_back) = address::parse_bech32(&c.bech32).expect("parse_bech32");
            assert_eq!(hrp_back, c.hrp);
            assert_eq!(payload_back, payload);

            // chain-prefixed "alias-bech32"
            let formatted = address::format(&c.alias, &c.hrp, &payload).expect("format");
            assert_eq!(formatted, c.formatted);
            let (alias, hrp2, payload2) = address::parse(&c.formatted).expect("parse");
            assert_eq!(alias, c.alias);
            assert_eq!(hrp2, c.hrp);
            assert_eq!(payload2, payload);
        }

        // no separator -> error
        assert!(address::parse("avax1qqqsyqcyq5rqwzq").is_err());
    }

    #[test]
    fn cb58_reexport_roundtrip() {
        // ava_crypto::cb58 re-exports ava_utils::cb58.
        let b = [0x01u8, 0x02, 0x03, 0x04];
        let s = ava_crypto::cb58::cb58_encode(&b).expect("cb58_encode");
        assert_eq!(ava_crypto::cb58::cb58_decode(&s).expect("cb58_decode"), b);
    }
}
