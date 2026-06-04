// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The object-safe BLS `Signer` trait.
//!
//! TODO(M0.21): `#[async_trait] Signer { public_key(&self) -> &PublicKey; sign;
//! sign_proof_of_possession; shutdown }`.
//! NOTE: `RpcSigner` (tonic over `proto/signer`) is DEFERRED to the milestone
//! owning proto codegen / `ava-vm-rpc` (M0 has no proto build).
//! Owning spec: `specs/25-key-management-and-signing.md` §3.1.
