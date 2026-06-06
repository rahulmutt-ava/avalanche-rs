// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.9 — NodeID-from-cert golden: `RIPEMD160(SHA256(DER))` parity with Go
//! `ids.NodeIDFromCert` (`specs/05` §1.6).

use ava_network::peer::upgrader::node_id_from_cert_der;
use serde::Deserialize;

#[derive(Deserialize)]
struct StakerVector {
    cert_der_hex: String,
    node_id: String,
}

#[test]
fn node_id_from_cert_golden() {
    let raw = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/tls/staker.json"
    ));
    let v: StakerVector = serde_json::from_str(raw).expect("parse staker.json");

    let der = hex::decode(&v.cert_der_hex).expect("decode cert DER hex");
    let node_id = node_id_from_cert_der(&der).expect("derive node id from cert");

    assert_eq!(
        node_id.to_string(),
        v.node_id,
        "NodeId::from_cert must equal Go ids.NodeIDFromCert"
    );
}
