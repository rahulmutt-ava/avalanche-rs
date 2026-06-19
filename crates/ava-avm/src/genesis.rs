// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `vms/avm/genesis.go` — the X-Chain genesis state: a list of genesis assets,
//! each an alias plus an embedded `CreateAssetTx`. Decoded/encoded with the
//! AVM **genesis codec** (`txs::codec::GenesisCodec`, `i32::MAX` slice cap).

use ava_codec::AvaCodec;

use crate::error::Result;
use crate::txs::CODEC_VERSION;
use crate::txs::CreateAssetTx;
use crate::txs::codec::GenesisCodec;

/// `avm.Genesis` — the X-Chain genesis state (`Txs []*GenesisAsset`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Genesis {
    /// The genesis assets, sorted by alias (the builder sorts; the parser
    /// preserves wire order).
    #[codec]
    pub txs: Vec<GenesisAsset>,
}

/// `avm.GenesisAsset` — an alias plus the embedded `CreateAssetTx` (the Go
/// struct embeds `txs.CreateAssetTx`, which serializes inline after `Alias`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct GenesisAsset {
    /// The asset alias (`"AVAX"` for the genesis fee asset).
    #[codec]
    pub alias: String,
    /// The embedded `txs.CreateAssetTx`.
    #[codec]
    pub tx: CreateAssetTx,
}

impl Genesis {
    /// `genesisCodec.Unmarshal(genesisBytes, &genesis)`.
    ///
    /// # Errors
    /// [`Error::Codec`](crate::error::Error::Codec) on malformed bytes.
    pub fn parse(bytes: &[u8]) -> Result<Genesis> {
        let mut genesis = Genesis::default();
        GenesisCodec().unmarshal(bytes, &mut genesis)?;
        Ok(genesis)
    }

    /// `genesisCodec.Marshal(txs.CodecVersion, g)`.
    ///
    /// # Errors
    /// [`Error::Codec`](crate::error::Error::Codec) on encode failure.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        Ok(GenesisCodec().marshal(CODEC_VERSION, self)?)
    }
}
