// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The unparsed (JSON) genesis config and its conversion to the parsed form
//! (`genesis/unparsed_config.go`, specs 23 §1).
//!
//! JSON stores `ethAddr` as `"0x"+hex(20 bytes)` and `avaxAddr` /
//! `rewardAddress` / `initialStakedFunds[i]` as bech32 `X-<hrp>1...`; parsing
//! strips the chain alias + HRP and decodes to a 20-byte [`ShortId`]. All
//! fields default to their zero value when absent (Go `encoding/json`
//! semantics).

use ava_crypto::address;
use ava_platformvm::signer::ProofOfPossession;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;

use crate::config::{Allocation, Config, LockedAmount, Staker};
use crate::error::{GenesisError, Result};

/// `unparsed_config.go::UnparsedAllocation`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct UnparsedAllocation {
    /// `ethAddr` — `0x`-prefixed hex, 20 bytes.
    #[serde(rename = "ethAddr")]
    pub eth_addr: String,
    /// `avaxAddr` — bech32 `X-<hrp>1...`.
    #[serde(rename = "avaxAddr")]
    pub avax_addr: String,
    /// `initialAmount`.
    #[serde(rename = "initialAmount")]
    pub initial_amount: u64,
    /// `unlockSchedule`.
    #[serde(rename = "unlockSchedule")]
    pub unlock_schedule: Vec<LockedAmount>,
}

impl UnparsedAllocation {
    /// `UnparsedAllocation.Parse`.
    ///
    /// # Errors
    /// [`GenesisError::InvalidEthAddress`] for a too-short `ethAddr`, else the
    /// hex/bech32/short-id decode error.
    pub fn parse(&self) -> Result<Allocation> {
        if self.eth_addr.len() < 2 {
            return Err(GenesisError::InvalidEthAddress);
        }
        // Go skips the first 2 chars without checking they are "0x".
        let eth_bytes =
            hex::decode(&self.eth_addr[2..]).map_err(|_| GenesisError::InvalidEthAddress)?;
        let eth_addr = ShortId::from_slice(&eth_bytes)?;
        let avax_addr = parse_avax_addr(&self.avax_addr)?;
        Ok(Allocation {
            eth_addr,
            avax_addr,
            initial_amount: self.initial_amount,
            unlock_schedule: self.unlock_schedule.clone(),
        })
    }
}

/// The JSON form of a BLS proof of possession (`signer.ProofOfPossession`
/// `MarshalJSON`): `publicKey` / `proofOfPossession` as `0x`-prefixed hex.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct UnparsedProofOfPossession {
    /// `publicKey` — 48-byte compressed BLS public key, `0x`-hex.
    #[serde(rename = "publicKey")]
    pub public_key: String,
    /// `proofOfPossession` — 96-byte BLS signature, `0x`-hex.
    #[serde(rename = "proofOfPossession")]
    pub proof_of_possession: String,
}

impl UnparsedProofOfPossession {
    /// Decodes the two `0x`-hex fields into a [`ProofOfPossession`].
    ///
    /// # Errors
    /// [`GenesisError::InvalidGenesisJson`] if either field is not `0x`-hex of
    /// the exact BLS length.
    pub fn parse(&self) -> Result<ProofOfPossession> {
        let pk = decode_0x_hex::<48>(&self.public_key)?;
        let sig = decode_0x_hex::<96>(&self.proof_of_possession)?;
        Ok(ProofOfPossession::new(pk, sig))
    }
}

/// Decodes a `0x`-prefixed hex string into an exact-length array.
fn decode_0x_hex<const N: usize>(s: &str) -> Result<[u8; N]> {
    let hex_str = s
        .strip_prefix("0x")
        .ok_or_else(|| GenesisError::InvalidGenesisJson(format!("missing 0x prefix: {s}")))?;
    let bytes = hex::decode(hex_str)
        .map_err(|e| GenesisError::InvalidGenesisJson(format!("bad hex: {e}")))?;
    <[u8; N]>::try_from(bytes).map_err(|v: Vec<u8>| {
        GenesisError::InvalidGenesisJson(format!("expected {N} bytes, got {}", v.len()))
    })
}

/// `unparsed_config.go::UnparsedStaker`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct UnparsedStaker {
    /// `nodeID` — `NodeID-<cb58>`.
    #[serde(rename = "nodeID")]
    pub node_id: NodeId,
    /// `rewardAddress` — bech32 `X-<hrp>1...`.
    #[serde(rename = "rewardAddress")]
    pub reward_address: String,
    /// `delegationFee` — millionths.
    #[serde(rename = "delegationFee")]
    pub delegation_fee: u32,
    /// `signer` — optional BLS proof of possession.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer: Option<UnparsedProofOfPossession>,
}

impl UnparsedStaker {
    /// `UnparsedStaker.Parse`.
    ///
    /// # Errors
    /// Propagates the bech32/short-id/PoP decode error.
    pub fn parse(&self) -> Result<Staker> {
        Ok(Staker {
            node_id: self.node_id,
            reward_address: parse_avax_addr(&self.reward_address)?,
            delegation_fee: self.delegation_fee,
            signer: self
                .signer
                .as_ref()
                .map(UnparsedProofOfPossession::parse)
                .transpose()?,
        })
    }
}

/// `unparsed_config.go::UnparsedConfig` — the JSON genesis config. Field names
/// are protocol constants and must match Go exactly.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct UnparsedConfig {
    /// `networkID`.
    #[serde(rename = "networkID")]
    pub network_id: u32,
    /// `allocations`.
    pub allocations: Vec<UnparsedAllocation>,
    /// `startTime`.
    #[serde(rename = "startTime")]
    pub start_time: u64,
    /// `initialStakeDuration`.
    #[serde(rename = "initialStakeDuration")]
    pub initial_stake_duration: u64,
    /// `initialStakeDurationOffset`.
    #[serde(rename = "initialStakeDurationOffset")]
    pub initial_stake_duration_offset: u64,
    /// `initialStakedFunds`.
    #[serde(rename = "initialStakedFunds")]
    pub initial_staked_funds: Vec<String>,
    /// `initialStakers`.
    #[serde(rename = "initialStakers")]
    pub initial_stakers: Vec<UnparsedStaker>,
    /// `cChainGenesis`.
    #[serde(rename = "cChainGenesis")]
    pub c_chain_genesis: String,
    /// `message`.
    pub message: String,
}

impl UnparsedConfig {
    /// `UnparsedConfig.Parse` — string addresses → [`ShortId`]s.
    ///
    /// # Errors
    /// Propagates the first address/signer decode error.
    pub fn parse(&self) -> Result<Config> {
        let mut config = Config {
            network_id: self.network_id,
            allocations: Vec::with_capacity(self.allocations.len()),
            start_time: self.start_time,
            initial_stake_duration: self.initial_stake_duration,
            initial_stake_duration_offset: self.initial_stake_duration_offset,
            initial_staked_funds: Vec::with_capacity(self.initial_staked_funds.len()),
            initial_stakers: Vec::with_capacity(self.initial_stakers.len()),
            c_chain_genesis: self.c_chain_genesis.clone(),
            message: self.message.clone(),
        };
        for ua in &self.allocations {
            config.allocations.push(ua.parse()?);
        }
        for isa in &self.initial_staked_funds {
            config.initial_staked_funds.push(parse_avax_addr(isa)?);
        }
        for uis in &self.initial_stakers {
            config.initial_stakers.push(uis.parse()?);
        }
        Ok(config)
    }
}

/// Parses a `<alias>-<hrp>1...` bech32 Avalanche address into its 20-byte
/// [`ShortId`] (Go `address.Parse` + `ids.ToShortID`).
fn parse_avax_addr(addr: &str) -> Result<ShortId> {
    let (_alias, _hrp, bytes) = address::parse(addr)?;
    Ok(ShortId::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M8.5 red test: `ethAddr`/`avaxAddr` string forms survive an
    /// unparse→parse round trip (specs 23 §1).
    #[test]
    fn ethaddr_avaxaddr_roundtrip() {
        let eth_bytes: [u8; 20] = [
            0xb3, 0xd8, 0x2b, 0x13, 0x67, 0xd3, 0x62, 0xde, 0x99, 0xab, 0x59, 0xa6, 0x58, 0x16,
            0x5a, 0xff, 0x52, 0x0c, 0xbd, 0x4d,
        ];
        let avax_bytes: [u8; 20] = [
            0x3c, 0xb7, 0xd3, 0x84, 0x2e, 0x8c, 0xee, 0x6a, 0x0e, 0xbd, 0x09, 0xf1, 0xfe, 0x88,
            0x4f, 0x68, 0x61, 0xe1, 0xb2, 0x9c,
        ];
        let avax_addr_str = address::format("X", "avax", &avax_bytes).expect("format avax addr");
        let ua = UnparsedAllocation {
            eth_addr: format!("0x{}", hex::encode(eth_bytes)),
            avax_addr: avax_addr_str.clone(),
            initial_amount: 42,
            unlock_schedule: vec![LockedAmount {
                amount: 7,
                locktime: 1_633_824_000,
            }],
        };
        let parsed = ua.parse().expect("parse allocation");
        assert_eq!(parsed.eth_addr.as_bytes(), &eth_bytes);
        assert_eq!(parsed.avax_addr.as_bytes(), &avax_bytes);
        assert_eq!(parsed.initial_amount, 42);

        // Re-format the parsed avaxAddr and assert string identity.
        let formatted = address::format("X", "avax", parsed.avax_addr.as_bytes()).expect("format");
        assert_eq!(formatted, avax_addr_str);

        // Too-short ethAddr mirrors Go errInvalidETHAddress.
        let bad = UnparsedAllocation {
            eth_addr: "0".to_string(),
            ..ua
        };
        assert_matches::assert_matches!(bad.parse(), Err(GenesisError::InvalidEthAddress));
    }
}
