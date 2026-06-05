// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The object-safe BLS `Signer` trait.
//!
//! Port of Go `utils/crypto/bls/signer.go::Signer` (`PublicKey`, `Sign`,
//! `SignProofOfPossession`, `Shutdown`). Every consumer holds an
//! `Arc<dyn Signer>` so the concrete key (local or remote) is hidden.
//!
//! **Sync vs async (deviation from `specs/25` §3.1):** the spec sketches an
//! `#[async_trait]` trait so a future remote/RPC signer can do blocking gRPC off
//! the async path. `async-trait` is not a dependency of this crate in M0 (no
//! RPC signer yet — that lands with proto codegen / `ava-vm-rpc`), and the plan
//! forbids adding a new workspace dependency here. The trait is therefore
//! synchronous for now; it can be made `async` when the RPC signer is added.
//!
//! `RpcSigner` (tonic over `proto/signer`) is DEFERRED to the milestone owning
//! proto codegen. Owning spec: `specs/25-key-management-and-signing.md` §3.1.

use super::keys::PublicKey;
use super::sign::Signature;
use crate::error::Result;

/// The object-safe BLS signer abstraction.
///
/// Mirrors `bls.Signer`. `Send + Sync` so it can live behind an `Arc` shared
/// across tasks/threads.
pub trait Signer: Send + Sync {
    /// The signer's BLS public key (G1).
    fn public_key(&self) -> &PublicKey;

    /// Sign `msg` with the SIGNATURE ciphersuite.
    ///
    /// # Errors
    /// Implementation-defined (a remote signer may fail to reach its backend).
    fn sign(&self, msg: &[u8]) -> Result<Signature>;

    /// Sign `msg` with the proof-of-possession ciphersuite.
    ///
    /// # Errors
    /// Implementation-defined (see [`Signer::sign`]).
    fn sign_proof_of_possession(&self, msg: &[u8]) -> Result<Signature>;

    /// Release any resources held by the signer. The default is a no-op (the
    /// in-process [`super::LocalSigner`] holds nothing to release).
    ///
    /// # Errors
    /// Implementation-defined.
    fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
