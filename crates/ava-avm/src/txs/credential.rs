// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `fxs.FxCredential` — a tx credential + its routing fx id (specs 09 §3.1).

use ava_codec::AvaCodec;
use ava_secp256k1fx::Credential as SecpCredential;
use ava_types::id::Id;

/// `verify.Verifiable` (fx credential) — the registered credential interface.
///
/// Marshals the typeID then the payload. All variants are structurally
/// `{ sigs: Vec<[u8; 65]> }` (nft/property embed secp's `Credential`), differing
/// only by type ID.
///
/// TODO(M5.5): add `nftfx.Credential` (14) and `propertyfx.Credential` (19); both
/// wrap a `secp256k1fx.Credential` and route via `TypeToFxIndex`.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Credential {
    /// `secp256k1fx.Credential` (type_id 9).
    #[codec(type_id = 9)]
    Secp256k1(SecpCredential),
}

impl Default for Credential {
    fn default() -> Self {
        Credential::Secp256k1(SecpCredential::default())
    }
}

/// `fxs.FxCredential` — a credential plus the fx id used to route it.
///
/// Wire layout (codec v0): just the typeid-prefixed `credential`. The `fx_id` is
/// **derived** (`serialize:"false"`) — filled in post-parse by looking up the
/// credential's concrete type in the fx routing table — so it carries no
/// `#[codec]` tag.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct FxCredential {
    /// The credential (interface; carries its own typeID).
    #[codec]
    pub credential: Credential,
    /// `FxID` — runtime-only (`serialize:"false"`); never encoded.
    pub fx_id: Id,
}

impl FxCredential {
    /// Builds an [`FxCredential`] over a concrete `secp256k1fx.Credential` with a
    /// routing fx id.
    #[must_use]
    pub fn new(fx_id: Id, credential: SecpCredential) -> Self {
        Self {
            credential: Credential::Secp256k1(credential),
            fx_id,
        }
    }

    /// Builds an [`FxCredential`] over an already-typed [`Credential`].
    #[must_use]
    pub fn with_credential(fx_id: Id, credential: Credential) -> Self {
        Self { credential, fx_id }
    }

    /// The routing fx id (`FxID`; runtime-only).
    #[must_use]
    pub fn fx_id(&self) -> Id {
        self.fx_id
    }
}
