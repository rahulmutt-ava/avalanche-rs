// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! File-backed `LocalSigner` (zeroized, 0o400 on disk).
//!
//! TODO(M0.21): `LocalSigner { sk: Zeroizing<SecretKeyBytes>, pk }` with
//! `generate/from_bytes/from_file/to_file/from_file_or_persist_new` — 32-byte
//! big-endian `SecretKey::serialize` file format (NOT PEM), `0o400`/`0o700`
//! perms, IKM + key zeroized. `Signer` impl routes `sign` -> SIG DST,
//! `sign_proof_of_possession` -> POP DST.
//! Owning spec: `specs/25-key-management-and-signing.md` §3.2, §6.
