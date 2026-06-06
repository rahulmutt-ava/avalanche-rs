// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

#![allow(unused_crate_dependencies)] // integration tests don't use every workspace dep (specs/01 §7.3)
#![allow(clippy::expect_used)] // tests assert via expect()

//! M2.9 — loopback mutual TLS 1.3 over `tokio::io::duplex`: both sides derive
//! the peer NodeID from its leaf cert (`specs/05` §1.6/§4.3).

use ava_network::Identity;
use ava_network::peer::tls_config::{client_config, server_config};
use ava_network::peer::upgrader::{Upgrader, node_id_from_cert_der};

#[tokio::test]
async fn loopback_mutual_tls_derives_node_id() {
    let server_id = Identity::generate().expect("server identity");
    let client_id = Identity::generate().expect("client identity");

    // Each side's own NodeID, derived from its own leaf cert.
    let server_node_id = node_id_from_cert_der(server_id.cert_der()).expect("server node id");
    let client_node_id = node_id_from_cert_der(client_id.cert_der()).expect("client node id");

    let server_cfg = server_config(&server_id).expect("server config");
    let client_cfg = client_config(&client_id).expect("client config");

    let (server_io, client_io) = tokio::io::duplex(64 * 1024);

    let server_upgrader = Upgrader::server(server_cfg);
    let client_upgrader = Upgrader::client(client_cfg);

    let server_task = tokio::spawn(async move { server_upgrader.upgrade(server_io).await });
    let client_task = tokio::spawn(async move { client_upgrader.upgrade(client_io).await });

    let (s_node, _s_stream, _s_cert) = server_task
        .await
        .expect("server task")
        .expect("server upgrade ok");
    let (c_node, _c_stream, _c_cert) = client_task
        .await
        .expect("client task")
        .expect("client upgrade ok");

    // The server sees the client's NodeID; the client sees the server's.
    assert_eq!(s_node, client_node_id, "server derives client's NodeID");
    assert_eq!(c_node, server_node_id, "client derives server's NodeID");
}

#[tokio::test]
async fn rejects_non_p256() {
    // A server presenting a P-384 leaf must fail the client's leaf-key policy,
    // so the handshake does not complete.
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P384_SHA384};

    // Build a P-384 identity by hand (rcgen + PEM round-trip through Identity).
    let key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("p384 key");
    let cert = CertificateParams::default()
        .self_signed(&key)
        .expect("self-sign p384");
    let bad_server = Identity::from_pem(&cert.pem(), &key.serialize_pem()).expect("p384 identity");

    let client_id = Identity::generate().expect("client identity");

    let server_cfg = server_config(&bad_server).expect("server config");
    let client_cfg = client_config(&client_id).expect("client config");

    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server_upgrader = Upgrader::server(server_cfg);
    let client_upgrader = Upgrader::client(client_cfg);

    let server_task = tokio::spawn(async move { server_upgrader.upgrade(server_io).await });
    let client_task = tokio::spawn(async move { client_upgrader.upgrade(client_io).await });

    let client_res = client_task.await.expect("client task");
    let server_res = server_task.await.expect("server task");

    assert!(
        client_res.is_err() || server_res.is_err(),
        "handshake with a non-P256 server leaf must fail"
    );
}
