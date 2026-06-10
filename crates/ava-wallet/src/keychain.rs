// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! secp256k1 keychain — port of `vms/secp256k1fx/keychain.go` (+ the eth-address
//! lookup `wallet/chain/c` needs).
//!
//! A [`Keychain`] holds private keys indexed by their AVAX short address (and by
//! their Ethereum address for C-Chain exports). [`Keychain::match_owners`]
//! mirrors `secp256k1fx.Keychain.Match` / `common.MatchOwners`: collect the sig
//! indices (in owner-address order) the keychain can sign for, up to the owner
//! threshold.

use std::collections::{BTreeMap, BTreeSet};

use ava_crypto::hashing;
use ava_crypto::secp256k1::{PrivateKey, SIGNATURE_LEN};
use ava_secp256k1fx::OutputOwners;
use ava_types::short_id::ShortId;

use crate::error::Result;

/// A set of secp256k1 private keys addressable by AVAX / Ethereum address.
#[derive(Default)]
pub struct Keychain {
    keys: Vec<PrivateKey>,
    by_addr: BTreeMap<ShortId, usize>,
    by_eth_addr: BTreeMap<[u8; 20], usize>,
}

impl Keychain {
    /// Builds a keychain from `keys` (Go `secp256k1fx.NewKeychain`).
    #[must_use]
    pub fn new(keys: Vec<PrivateKey>) -> Self {
        let mut kc = Keychain::default();
        for key in keys {
            kc.add(key);
        }
        kc
    }

    /// `Keychain.Add` — registers a key under its AVAX + eth addresses.
    pub fn add(&mut self, key: PrivateKey) {
        let pk = key.public_key();
        let addr = pk.address();
        let eth = pk.eth_address();
        let idx = self.keys.len();
        self.keys.push(key);
        self.by_addr.entry(addr).or_insert(idx);
        self.by_eth_addr.entry(eth).or_insert(idx);
    }

    /// `Keychain.Get` — the signer for `addr`, if owned.
    #[must_use]
    pub fn get(&self, addr: &ShortId) -> Option<&PrivateKey> {
        self.by_addr.get(addr).map(|i| &self.keys[*i])
    }

    /// `EthKeychain.GetEth` — the signer for the eth address, if owned.
    #[must_use]
    pub fn get_eth(&self, addr: &[u8; 20]) -> Option<&PrivateKey> {
        self.by_eth_addr.get(addr).map(|i| &self.keys[*i])
    }

    /// `Keychain.Addresses` — the AVAX addresses this keychain controls.
    #[must_use]
    pub fn addresses(&self) -> BTreeSet<ShortId> {
        self.by_addr.keys().copied().collect()
    }

    /// `EthKeychain.EthAddresses`.
    #[must_use]
    pub fn eth_addresses(&self) -> BTreeSet<[u8; 20]> {
        self.by_eth_addr.keys().copied().collect()
    }

    /// `Keychain.Match` — the sig indices (in owner order) this keychain can
    /// satisfy, or `None` if the owner is timelocked past `min_issuance_time` or
    /// the threshold cannot be met.
    #[must_use]
    pub fn match_owners(&self, owners: &OutputOwners, min_issuance_time: u64) -> Option<Vec<u32>> {
        crate::common::match_owners(owners, &self.addresses(), min_issuance_time)
    }
}

/// `keychain.Signer.Sign` — a recoverable secp256k1 signature over
/// `sha256(msg)` (Go `PrivateKey.Sign`).
///
/// # Errors
/// Propagates the underlying signing failure.
pub fn sign_msg(key: &PrivateKey, msg: &[u8]) -> Result<[u8; SIGNATURE_LEN]> {
    let hash = hashing::sha256(msg);
    Ok(key.sign_hash(&hash)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> PrivateKey {
        // `secp256k1.TestKeys()[1]` (cb58
        // 2MMvUMsxx6zsHSNXJdFD8yc5XkancvwyKPwpw4xUK3TCGDuNBY).
        let bytes =
            ava_crypto::cb58::cb58_decode("2MMvUMsxx6zsHSNXJdFD8yc5XkancvwyKPwpw4xUK3TCGDuNBY")
                .expect("decode test key");
        PrivateKey::from_bytes(&bytes).expect("test key")
    }

    #[test]
    fn keychain_lookup_and_match() {
        let key = test_key();
        let addr = key.public_key().address();
        let kc = Keychain::new(vec![key]);

        assert!(kc.get(&addr).is_some());
        assert_eq!(kc.addresses(), BTreeSet::from([addr]));

        let owners = OutputOwners::new(0, 1, vec![addr]);
        assert_eq!(kc.match_owners(&owners, 0), Some(vec![0]));

        // Timelocked past min_issuance_time -> no match.
        let locked = OutputOwners::new(100, 1, vec![addr]);
        assert_eq!(kc.match_owners(&locked, 99), None);
        assert_eq!(kc.match_owners(&locked, 100), Some(vec![0]));

        // Threshold unsatisfiable -> no match.
        let other = OutputOwners::new(0, 2, vec![addr, ShortId::EMPTY]);
        assert_eq!(kc.match_owners(&other, 0), None);
    }
}
