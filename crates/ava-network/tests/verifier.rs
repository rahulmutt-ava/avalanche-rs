// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

#![allow(unused_crate_dependencies)] // integration tests don't use every workspace dep (specs/01 §7.3)
#![allow(clippy::expect_used)] // tests assert via expect()

//! M2.8 — leaf-key policy: P-256 only for ECDSA; well-formed RSA; reject the
//! rest (`specs/05` §1.6/§4.5). Mirrors Go `ValidateCertificate`.

use assert_matches::assert_matches;
use ava_network::Error;
use ava_network::peer::verifier::validate_leaf_public_key;
use rcgen::{
    CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256, PKCS_ECDSA_P384_SHA384, PKCS_ED25519,
};
use rustls::pki_types::CertificateDer;

/// A small (1024-bit) RSA self-signed cert (DER), generated offline via openssl
/// (ring cannot generate RSA keys). Modulus is 1024 bits < the 2048 minimum, so
/// the well-formed RSA policy must reject it.
const RSA_1024_DER_HEX: &str = "308201dc30820145a003020102021401f18e08f6807dc3e64c857ba04fcb8b9413dc5b300d06092a864886f70d01010b05003000301e170d3236303630363137333735365a170d3336303630333137333735365a300030819f300d06092a864886f70d010101050003818d0030818902818100981ec36c53dd69ae94d139ccccf3a2ac036b5ea100822a2a58d9e92fe9ee42c2509730c6146dae8c87a650650cd3396e7d3031e3532c614afe59954f924449b7f3c79da7c66b5f53d357e44357132367666278564dec63564ca72326b9e23f05654ff30cb6756683f58f64d5da2e15950de3066ad73e60e0a0bd6f2bc43a41bf0203010001a3533051301d0603551d0e0416041485ced250012aa1d25eb85cc7badcb96bedf476cb301f0603551d2304183016801485ced250012aa1d25eb85cc7badcb96bedf476cb300f0603551d130101ff040530030101ff300d06092a864886f70d01010b050003818100063f51f046337c6091230e8e0d79a1e24140887dd205e0b5d079d55c21a7b9d8039cb75a5b47b24d93ebc7167a29d50272a2b9e1fca6a8c7b0cba22729ab61df4100edbfa9676d9eadfd922cee6d11c2b1ade67700f6b3d076491ceec1afec84255827bfdb3d434a586cb6f50107139a6a73a42d3bee0b5fd0284ac08d225a95";

/// Generate a self-signed cert DER with the given rcgen signature algorithm.
fn self_signed_der(alg: &'static rcgen::SignatureAlgorithm) -> Vec<u8> {
    let key = KeyPair::generate_for(alg).expect("generate key");
    let params = CertificateParams::default();
    let cert = params.self_signed(&key).expect("self-sign");
    cert.der().to_vec()
}

#[test]
fn accepts_p256_rejects_others() {
    // P-256 ECDSA — accepted.
    let p256 = self_signed_der(&PKCS_ECDSA_P256_SHA256);
    validate_leaf_public_key(&CertificateDer::from(p256)).expect("P-256 leaf is accepted");

    // P-384 ECDSA — wrong curve.
    let p384 = self_signed_der(&PKCS_ECDSA_P384_SHA384);
    assert_matches!(
        validate_leaf_public_key(&CertificateDer::from(p384)),
        Err(Error::CurveMismatch)
    );

    // Ed25519 — unsupported key type.
    let ed = self_signed_der(&PKCS_ED25519);
    assert_matches!(
        validate_leaf_public_key(&CertificateDer::from(ed)),
        Err(Error::UnsupportedKeyType)
    );

    // RSA with a 1024-bit modulus — rejected by the well-formed RSA policy.
    let rsa = hex::decode(RSA_1024_DER_HEX).expect("decode rsa der");
    assert_matches!(validate_leaf_public_key(&CertificateDer::from(rsa)), Err(_));
}
