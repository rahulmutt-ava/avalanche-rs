# TLS test fixtures

`staker_rsa_v1.{crt,key}` — vendored verbatim from avalanchego's public local
network stakers (`staking/local/staker1.{crt,key}`). It is a **self-signed
X.509 v1 RSA-4096** certificate (public exponent 65537), the exact shape
avalanchego mints for RSA staking identities. webpki rejects v1 certs with
`UnsupportedCertVersion`; this fixture reproduces that case so the
`tls_v1_rsa_handshake` tests prove our verifiers accept it via the raw-key
signature path (`specs/05` §4.4/§4.5). These are test-only local-network keys,
never used on a real network.
