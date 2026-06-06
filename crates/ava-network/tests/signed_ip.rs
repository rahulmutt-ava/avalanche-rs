// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

#![allow(unused_crate_dependencies)] // integration tests don't use every workspace dep (specs/01 §7.3)
#![allow(clippy::expect_used)] // tests assert via expect()

//! M2.10 — IP signing: UnsignedIP byte layout + SignedIp TLS-sig verify
//! (`specs/05` §1.6/§3.5, `specs/15` §4.1).

use std::net::{IpAddr, Ipv4Addr};

use assert_matches::assert_matches;
use ava_crypto::bls::LocalSigner;
use ava_crypto::staking::parse_certificate;
use ava_network::Error;
use ava_network::Identity;
use ava_network::peer::ip::UnsignedIp;
use serde::Deserialize;

#[derive(Deserialize)]
struct IpVector {
    ip: String,
    port: u16,
    timestamp: u64,
    bytes_hex: String,
    as16_hex: String,
}

#[test]
fn unsigned_ip_bytes_layout() {
    let raw = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/message/signed_ip.json"
    ));
    let v: IpVector = serde_json::from_str(raw).expect("parse signed_ip.json");

    let ip: IpAddr = v.ip.parse().expect("parse ip");
    let unsigned = UnsignedIp::new(ip, v.port, v.timestamp);

    assert_eq!(
        hex::encode(unsigned.bytes()),
        v.bytes_hex,
        "UnsignedIp::bytes() must equal ip.As16()(16) || port_be(2) || ts_be(8)"
    );
    assert_eq!(
        hex::encode(unsigned.addr_as16()),
        v.as16_hex,
        "addr_as16 must be the IPv4-mapped IPv6 form"
    );
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

#[test]
fn signed_ip_verify_roundtrip() {
    let identity = Identity::generate().expect("identity");
    let tls_signer = identity.tls_signing_key().expect("tls signer");
    let bls_signer = LocalSigner::generate().expect("bls signer");
    let cert = parse_certificate(identity.cert_der()).expect("parse own cert");

    let now = now_unix();
    let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let unsigned = UnsignedIp::new(ip, 9651, now.saturating_sub(1));

    let signed = unsigned
        .sign(&tls_signer, &bls_signer)
        .expect("sign unsigned ip");

    // Valid: timestamp before max (now + 60s).
    let max = now.saturating_add(60);
    signed.verify(&cert, max).expect("valid signed ip verifies");

    // Timestamp too far in the future.
    let future = UnsignedIp::new(ip, 9651, now.saturating_add(120));
    let signed_future = future.sign(&tls_signer, &bls_signer).expect("sign future");
    assert_matches!(
        signed_future.verify(&cert, max),
        Err(Error::TimestampTooFarInFuture)
    );

    // Tampered TLS signature -> invalid.
    let mut tampered = signed.clone();
    tampered.corrupt_tls_signature_for_test();
    assert_matches!(tampered.verify(&cert, max), Err(Error::InvalidTlsSignature));
}

#[test]
fn bls_proof_of_possession_over_raw_bytes() {
    use ava_crypto::bls::{Signature, Signer, verify_pop};

    let bls_signer = LocalSigner::generate().expect("bls signer");
    let identity = Identity::generate().expect("identity");
    let tls_signer = identity.tls_signing_key().expect("tls signer");

    let ip = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));
    let unsigned = UnsignedIp::new(ip, 1, 1_600_000_000);
    let signed = unsigned.sign(&tls_signer, &bls_signer).expect("sign");

    // The BLS PoP is over the RAW ip bytes (not the SHA256 digest).
    let sig = Signature::from_bytes(signed.bls_signature_bytes()).expect("parse bls sig");
    assert!(
        verify_pop(bls_signer.public_key(), &sig, &unsigned.bytes()),
        "BLS PoP must verify over raw UnsignedIp bytes"
    );
}
