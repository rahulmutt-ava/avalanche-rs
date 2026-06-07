// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M4.15 TDD entry tests for the UTXO handler + ATOMIC-1 fx registration
//! (`vms/platformvm/utxo`, `vms/components/avax`, specs 08 §2.4 / §1; 00 §11.1.7).
//!
//! - [`golden::atomic_utxo_decode`] — ATOMIC-1: an `avax.UTXO` carrying a
//!   `secp256k1fx::TransferOutput` produced under the P-Chain fx registration
//!   marshals byte-identically to the canonical cross-chain wire layout (codec
//!   version `0x0000` + `UTXOID{txID, outputIndex}` + `AssetID` + typed fx output
//!   at the AVM-aligned type_id 7), and round-trips.
//! - [`prop::spend_produce_balances`] — `sum(consumed_in) == sum(produced_out) +
//!   fee` through the handler over a `state::Diff`.

// This suite exercises only the UTXO subsystem; the dev-deps declared for the
// codec/reward suites are not all used here.
#![allow(unused_crate_dependencies)]
// Integration-test helpers are not recognized by clippy's allow-in-tests
// heuristic; these are test-only and harmless.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]

use std::sync::Arc;

use ava_database::MemDb;
use ava_platformvm::state::chain::{Chain, Versions};
use ava_platformvm::state::diff::Diff;
use ava_platformvm::state::state::State;
use ava_platformvm::txs::components::{
    Input as TxInput, Output, TransferableInput, TransferableOutput,
};
use ava_platformvm::utxo::Utxo;
use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

/// A `Versions` that resolves exactly one parent block ID to a shared `Chain`.
struct SingleParent {
    id: Id,
    chain: Arc<dyn Chain>,
}

impl Versions for SingleParent {
    fn get_state(&self, block_id: Id) -> Option<Arc<dyn Chain>> {
        if block_id == self.id {
            Some(Arc::clone(&self.chain))
        } else {
            None
        }
    }
}

mod golden {
    use super::*;

    /// ATOMIC-1: a P-Chain `avax.UTXO` with a `secp256k1fx.TransferOutput`
    /// marshals byte-identically to the canonical cross-chain wire layout, so a
    /// UTXO exported by one chain decodes on another (registered type_ids align).
    #[test]
    fn atomic_utxo_decode() {
        let tx_id = Id::from([0x11; 32]);
        let output_index: u32 = 7;
        let asset_id = Id::from([0x22; 32]);
        let addr = ShortId::from([0x33; 20]);

        let owners = OutputOwners::new(0, 1, vec![addr]);
        let out = TransferOutput::new(123_456, owners);
        let utxo = Utxo {
            tx_id,
            output_index,
            asset_id,
            out: Output::Transfer(out),
        };

        let got = utxo.marshal().expect("marshal utxo");

        // Hand-built canonical avax.UTXO wire layout (proves the type_ids align):
        //   codec version 0x0000
        //   UTXOID: txID(32) || outputIndex(u32 BE)
        //   Asset:  assetID(32)
        //   Out interface: typeID(u32 BE = 7) || TransferOutput fields
        //     amt(u64 BE) || locktime(u64 BE) || threshold(u32 BE)
        //       || addrs len(u32 BE) || addr(20)
        let mut want = Vec::new();
        want.extend_from_slice(&[0x00, 0x00]); // codec version
        want.extend_from_slice(&[0x11; 32]); // txID
        want.extend_from_slice(&7u32.to_be_bytes()); // outputIndex
        want.extend_from_slice(&[0x22; 32]); // assetID
        want.extend_from_slice(&7u32.to_be_bytes()); // type_id = secp256k1fx.TransferOutput
        want.extend_from_slice(&123_456u64.to_be_bytes()); // amt
        want.extend_from_slice(&0u64.to_be_bytes()); // locktime
        want.extend_from_slice(&1u32.to_be_bytes()); // threshold
        want.extend_from_slice(&1u32.to_be_bytes()); // addrs len
        want.extend_from_slice(&[0x33; 20]); // addr

        assert_eq!(got, want, "UTXO wire layout must match canonical avax.UTXO");

        // Round-trips byte-identically.
        let back = Utxo::unmarshal(&got).expect("unmarshal utxo");
        assert_eq!(back, utxo);
        assert_eq!(back.marshal().expect("re-marshal"), want);
    }
}

mod prop {
    use super::*;

    /// Build a single-output, single-input balanced spend and assert the handler
    /// enforces `sum(consumed_in) == sum(produced_out) + fee` over a `Diff`.
    #[test]
    fn spend_produce_balances() {
        let parent_id = Id::from([0xAA; 32]);
        let base = State::new(MemDb::new()).expect("state");
        let versions = SingleParent {
            id: parent_id,
            chain: Arc::new(base) as Arc<dyn Chain>,
        };
        let mut diff = Diff::new(parent_id, &versions).expect("diff");

        let asset_id = Id::from([0x22; 32]);
        let addr = ShortId::from([0x33; 20]);
        let owners = OutputOwners::new(0, 1, vec![addr]);

        // Produce a UTXO worth 1000 from a "funding" tx, into the diff.
        let funding_tx = Id::from([0x01; 32]);
        let produced = vec![TransferableOutput {
            asset_id,
            out: Output::Transfer(TransferOutput::new(1000, owners.clone())),
        }];
        ava_platformvm::utxo::produce(&mut diff, funding_tx, &produced)
            .expect("produce funding utxo");

        // Spend it: 1 input (1000), 1 output (900), fee 100 ⇒ balanced.
        let ins = vec![TransferableInput {
            tx_id: funding_tx,
            output_index: 0,
            asset_id,
            r#in: TxInput::Transfer(TransferInput::new(1000, vec![0])),
        }];
        let outs = vec![TransferableOutput {
            asset_id,
            out: Output::Transfer(TransferOutput::new(900, owners.clone())),
        }];
        let fee = 100u64;

        // The handler checks the balance equation and the UTXOs exist.
        ava_platformvm::utxo::verify_spend(&diff, &ins, &outs, fee, asset_id)
            .expect("balanced spend verifies");

        // Off-by-one breaks it: fee 101 ⇒ produced + fee > consumed.
        let err = ava_platformvm::utxo::verify_spend(&diff, &ins, &outs, 101, asset_id);
        assert!(err.is_err(), "unbalanced spend must be rejected");

        // Apply the state transition: consume inputs, produce outputs.
        let spend_tx = Id::from([0x02; 32]);
        ava_platformvm::utxo::consume(&mut diff, &ins);
        ava_platformvm::utxo::produce(&mut diff, spend_tx, &outs).expect("produce change");

        // The consumed UTXO is gone; the new one exists.
        let consumed_id = ins[0].input_id();
        assert!(diff.get_utxo(consumed_id).is_err(), "consumed utxo deleted");
        let new_id = Id::from([0x02; 32]).prefix(&[0]);
        assert!(diff.get_utxo(new_id).is_ok(), "produced utxo present");
    }
}
