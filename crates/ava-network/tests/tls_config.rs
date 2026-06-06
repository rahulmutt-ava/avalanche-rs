// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.7 — TLS configs are TLS1.3-only, mutual, no ALPN (`specs/05` §4.1/§4.2).

use ava_network::Identity;
use ava_network::peer::tls_config::{
    client_config, enabled_protocol_versions, server_config, server_requires_client_cert,
};

#[test]
fn configs_are_tls13_only_and_mutual() {
    let identity = Identity::generate().expect("generate staking identity");

    let server = server_config(&identity).expect("server config");
    let client = client_config(&identity).expect("client config");

    // TLS 1.3 ONLY: the enabled-version set is exactly `[TLS13]`.
    let versions = enabled_protocol_versions();
    assert_eq!(versions.len(), 1, "exactly one protocol version enabled");
    let only = versions.first().expect("one enabled version");
    assert_eq!(
        only.version,
        rustls::version::TLS13.version,
        "the only enabled version must be TLS 1.3"
    );

    // Mutual auth: the server requires a client certificate.
    assert!(
        server_requires_client_cert(),
        "server must require a client cert (RequireAnyClientCert)"
    );

    // No ALPN on either side (Go sets none).
    assert!(
        server.alpn_protocols.is_empty(),
        "server must not advertise ALPN"
    );
    assert!(
        client.alpn_protocols.is_empty(),
        "client must not advertise ALPN"
    );
}
