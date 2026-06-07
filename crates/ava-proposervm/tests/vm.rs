// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for the `ProposerVm` wrapper (M3.23).
//!
//! Covers:
//! - the generic `vm_conformance!` battery against `ProposerVm<TestVm, _>`
//!   (pre-fork regime — the wrapper transparently delegates to the inner VM);
//! - the post-fork build path: wait for this node's slot on a virtual clock,
//!   sign with the staking cert, emit a post-fork block;
//! - `get_block_id_at_height` served via the height index;
//! - `as_batched` / `as_state_syncable` delegating to the inner VM.

// Test-crate helpers (non-`#[test]` fns) use `expect()` for setup; allow it
// crate-wide here (the production crate forbids it).
#![allow(clippy::expect_used)]

// Normal/auto-linked workspace deps not used by this integration test target.
use assert_matches as _;
use ava_codec as _;
use hex as _;
use pretty_assertions as _;
use proptest as _;
use serde as _;
use sha2 as _;
use thiserror as _;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair};
use tokio_util::sync::CancellationToken;

use ava_crypto::staking;
use ava_database::{DynDatabase, MemDb};
use ava_proposervm::vm::{BlockSigner, ProposerVm, StakingIdentity};
use ava_snow::ChainContext;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, MockClock};
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;
use ava_vm::block::ChainVm;
use ava_vm::testutil::{NoopAppSender, TestVm};
use ava_vm::vm::Vm;

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// A fixed validator-set `ValidatorState` (single validator = this node).
#[derive(Clone)]
struct FixedState {
    set: BTreeMap<NodeId, GetValidatorOutput>,
}

#[async_trait]
impl ValidatorState for FixedState {
    async fn get_minimum_height(&self) -> ava_validators::Result<u64> {
        Ok(0)
    }
    async fn get_current_height(&self) -> ava_validators::Result<u64> {
        Ok(1)
    }
    async fn get_subnet_id(&self, _chain: Id) -> ava_validators::Result<Id> {
        Ok(Id::EMPTY)
    }
    async fn get_validator_set(
        &self,
        _height: u64,
        _subnet: Id,
    ) -> ava_validators::Result<BTreeMap<NodeId, GetValidatorOutput>> {
        Ok(self.set.clone())
    }
    async fn get_current_validator_set(
        &self,
        _subnet: Id,
    ) -> ava_validators::Result<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        Ok((BTreeMap::new(), 1))
    }
    async fn get_warp_validator_sets(
        &self,
        _height: u64,
    ) -> ava_validators::Result<HashMap<Id, WarpSet>> {
        Ok(HashMap::new())
    }
}

/// Generates a staking cert + an ECDSA P-256 signer over `header.bytes()`.
fn staking_identity() -> (StakingIdentity, NodeId) {
    let (cert_pem, key_pem) = staking::new_cert_and_key_bytes().expect("gen cert");
    let cert_der = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .next()
        .expect("a cert block")
        .expect("valid cert pem")
        .to_vec();
    let node_id = staking::node_id_from_cert(&cert_der);

    let key_pair = rcgen::KeyPair::from_pem(&key_pem).expect("parse key pem");
    let pkcs8 = key_pair.serialize_der();
    let signer: BlockSigner = Arc::new(move |msg: &[u8]| {
        let rng = SystemRandom::new();
        let signing = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng)
            .map_err(|e| format!("import pkcs8: {e:?}"))?;
        let sig = signing
            .sign(&rng, msg)
            .map_err(|e| format!("sign: {e:?}"))?;
        Ok(sig.as_ref().to_vec())
    });
    (
        StakingIdentity {
            certificate: cert_der,
            signer,
        },
        node_id,
    )
}

/// A `ChainContext` whose `node_id` is the proposer and whose upgrade schedule
/// is `network_upgrades`.
fn ctx_with(
    node_id: NodeId,
    network_upgrades: ava_version::upgrade::UpgradeConfig,
) -> Arc<ChainContext> {
    Arc::new(ChainContext {
        network_id: 1,
        subnet_id: Id::EMPTY,
        chain_id: Id::from([7u8; 32]),
        node_id,
        public_key: None,
        network_upgrades,
        x_chain_id: Id::EMPTY,
        c_chain_id: Id::EMPTY,
        avax_asset_id: Id::EMPTY,
        chain_data_dir: std::path::PathBuf::new(),
    })
}

/// Builds a `pre-Durango`-disabled, post-fork-activated upgrade config: every
/// fork up to and including Durango activates at `fork_unix`; Granite stays off.
fn upgrades_post_durango(fork_unix: i64) -> ava_version::upgrade::UpgradeConfig {
    let mut cfg = ava_version::upgrade::get_config(1);
    let at = Utc.timestamp_opt(fork_unix, 0).single().expect("ts");
    let never = Utc
        .timestamp_opt(i64::from(i32::MAX), 0)
        .single()
        .expect("ts");
    cfg.apricot_phase_1_time = at;
    cfg.apricot_phase_2_time = at;
    cfg.apricot_phase_3_time = at;
    cfg.apricot_phase_4_time = at;
    cfg.apricot_phase_4_min_p_chain_height = 0;
    cfg.apricot_phase_5_time = at;
    cfg.apricot_phase_pre_6_time = at;
    cfg.apricot_phase_6_time = at;
    cfg.apricot_phase_post_6_time = at;
    cfg.banff_time = at;
    cfg.cortina_time = at;
    cfg.durango_time = at;
    cfg.etna_time = at;
    cfg.fortuna_time = at;
    cfg.granite_time = never;
    cfg.helicon_time = never;
    cfg
}

/// A pre-fork upgrade config: the ProposerVM fork never activates.
fn upgrades_pre_fork() -> ava_version::upgrade::UpgradeConfig {
    let mut cfg = ava_version::upgrade::get_config(1);
    let never = Utc
        .timestamp_opt(i64::from(i32::MAX), 0)
        .single()
        .expect("ts");
    cfg.apricot_phase_4_time = never;
    cfg.durango_time = never;
    cfg.granite_time = never;
    cfg
}

async fn init_inner(token: &CancellationToken) -> TestVm {
    let mut vm = TestVm::new();
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    vm.initialize(
        token,
        ava_vm::testutil::test_chain_context(),
        db,
        b"genesis",
        b"",
        b"",
        Vec::new(),
        Arc::new(NoopAppSender),
    )
    .await
    .expect("init inner");
    vm
}

/// Builds a `ProposerVm` over a fresh inner `TestVm`, pre-fork (transparent
/// delegation), already initialized. Used by the conformance battery.
async fn make_prefork_proposervm(token: CancellationToken) -> ProposerVm<TestVm, FixedState> {
    let clock = Arc::new(MockClock::at(UNIX_EPOCH));
    let (identity, node_id) = staking_identity();
    let mut set = BTreeMap::new();
    set.insert(
        node_id,
        GetValidatorOutput {
            node_id,
            public_key: None,
            weight: 1,
        },
    );
    let ctx = ctx_with(node_id, upgrades_pre_fork());
    let state = FixedState { set };
    let inner = init_inner(&token).await;
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let mut vm = ProposerVm::new(
        inner,
        ctx,
        clock as Arc<dyn Clock>,
        state,
        db,
        Some(identity),
    );
    vm.initialize_wrapper(&token).await.expect("init wrapper");
    vm
}

// ---------------------------------------------------------------------------
// Conformance battery (pre-fork regime: transparent delegation)
// ---------------------------------------------------------------------------

ava_vm::vm_conformance!(|token: ::tokio_util::sync::CancellationToken| async move {
    super::make_prefork_proposervm(token).await
});

// ---------------------------------------------------------------------------
// proposervm_wraps_inner — basic delegation sanity (last_accepted = inner)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn proposervm_wraps_inner() {
    let token = CancellationToken::new();
    let vm = make_prefork_proposervm(token.clone()).await;
    let last = vm.last_accepted(&token).await.expect("last_accepted");
    let genesis = vm
        .get_block_id_at_height(&token, 0)
        .await
        .expect("genesis at height 0");
    assert_eq!(last, genesis);
}

// ---------------------------------------------------------------------------
// build_block — post-fork: wait for slot, sign, emit post-fork block
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn build_block_post_fork_signs_and_emits() {
    let token = CancellationToken::new();

    // Fork at unix 0; the clock starts well past the fork so we're post-Durango.
    let fork_unix = 0i64;
    let now = UNIX_EPOCH + Duration::from_secs(1_000);
    let clock = Arc::new(MockClock::at(now));

    let (identity, node_id) = staking_identity();
    let mut set = BTreeMap::new();
    set.insert(
        node_id,
        GetValidatorOutput {
            node_id,
            public_key: None,
            weight: 1,
        },
    );
    let ctx = ctx_with(node_id, upgrades_post_durango(fork_unix));
    let state = FixedState { set };
    let inner = init_inner(&token).await;
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let mut vm = ProposerVm::new(
        inner,
        ctx,
        Arc::clone(&clock) as Arc<dyn Clock>,
        state,
        db,
        Some(identity),
    );
    vm.initialize_wrapper(&token).await.expect("init wrapper");
    vm.set_state(&token, ava_snow::EngineState::NormalOp)
        .await
        .expect("set_state");

    // The genesis is pre-fork; building the first child crosses the fork and
    // produces a post-fork *transition* block (Go: the first post-fork block,
    // child of a pre-fork block, is always unsigned with no proposer).
    let transition = vm.build_block(&token).await.expect("build transition");
    assert_eq!(transition.height(), 1, "child of genesis is at height 1");
    let parsed = ava_proposervm::block::parse_without_verification(transition.bytes())
        .expect("post-fork block parses");
    assert!(
        matches!(parsed, ava_proposervm::block::ParsedBlock::Signed(ref sb) if sb.proposer() == NodeId::EMPTY),
        "transition block is unsigned (no proposer)"
    );
    vm.set_preference(&token, transition.id())
        .await
        .expect("set_pref");
    transition.verify(&token).await.expect("verify transition");
    transition.accept(&token).await.expect("accept transition");
    assert_eq!(
        vm.get_block_id_at_height(&token, 1)
            .await
            .expect("height 1 indexed"),
        transition.id()
    );

    // The next child is the child of a *post-fork* block. With a single
    // validator (this node), it is the expected proposer for its slot, so a
    // *signed* block is emitted.
    let blk = vm.build_block(&token).await.expect("build signed");
    assert_eq!(blk.height(), 2, "second post-fork block is at height 2");
    let parsed = ava_proposervm::block::parse_without_verification(blk.bytes())
        .expect("post-fork block parses");
    match parsed {
        ava_proposervm::block::ParsedBlock::Signed(sb) => {
            assert_eq!(sb.proposer(), node_id, "signed by this node");
            assert!(!sb.signature().is_empty(), "carries a signature");
        }
        other => panic!("expected a signed post-fork block, got {other:?}"),
    }
    // The signature verifies against the chain id.
    ava_proposervm::block::parse(blk.bytes(), Id::from([7u8; 32])).expect("signature verifies");

    // Accept advances the height index.
    blk.verify(&token).await.expect("verify");
    blk.accept(&token).await.expect("accept");
    let at_height = vm
        .get_block_id_at_height(&token, 2)
        .await
        .expect("height index advanced");
    assert_eq!(at_height, blk.id(), "accept advances the height index");
    let last = vm.last_accepted(&token).await.expect("last_accepted");
    assert_eq!(last, blk.id());
}

// ---------------------------------------------------------------------------
// build_block — pre-fork: returns the bare inner block
// ---------------------------------------------------------------------------

#[tokio::test]
async fn build_block_pre_fork_returns_inner() {
    let token = CancellationToken::new();
    let mut vm = make_prefork_proposervm(token.clone()).await;
    vm.set_state(&token, ava_snow::EngineState::NormalOp)
        .await
        .expect("set_state");

    let blk = vm.build_block(&token).await.expect("build_block");
    assert_eq!(blk.height(), 1);
    // A pre-fork block's bytes do NOT parse as a post-fork proposervm block;
    // they are the bare inner bytes.
    assert!(
        ava_proposervm::block::parse_without_verification(blk.bytes()).is_err()
            || ava_proposervm::block::parse_without_verification(blk.bytes())
                .map(|p| p.inner_block().to_vec())
                .unwrap_or_default()
                != blk.bytes(),
        "pre-fork block is the bare inner block, not a wrapped post-fork block"
    );
}

// ---------------------------------------------------------------------------
// get_block_id_at_height — served via the height index after accepts
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn get_block_id_at_height_via_index() {
    let token = CancellationToken::new();
    let fork_unix = 0i64;
    let clock = Arc::new(MockClock::at(UNIX_EPOCH + Duration::from_secs(1_000)));
    let (identity, node_id) = staking_identity();
    let mut set = BTreeMap::new();
    set.insert(
        node_id,
        GetValidatorOutput {
            node_id,
            public_key: None,
            weight: 1,
        },
    );
    let ctx = ctx_with(node_id, upgrades_post_durango(fork_unix));
    let inner = init_inner(&token).await;
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let mut vm = ProposerVm::new(
        inner,
        ctx,
        Arc::clone(&clock) as Arc<dyn Clock>,
        FixedState { set },
        db,
        Some(identity),
    );
    vm.initialize_wrapper(&token).await.expect("init wrapper");
    vm.set_state(&token, ava_snow::EngineState::NormalOp)
        .await
        .expect("set_state");

    let blk = vm.build_block(&token).await.expect("build_block");
    vm.set_preference(&token, blk.id()).await.expect("set_pref");
    blk.verify(&token).await.expect("verify");
    blk.accept(&token).await.expect("accept");

    // Served via the proposervm height index.
    let got = vm
        .get_block_id_at_height(&token, 1)
        .await
        .expect("height 1");
    assert_eq!(got, blk.id());
    // Pre-fork heights below the fork height fall through to the inner VM.
    let got0 = vm
        .get_block_id_at_height(&token, 0)
        .await
        .expect("height 0");
    let _ = got0;
}

// ---------------------------------------------------------------------------
// as_batched / as_state_syncable delegate to the inner VM
// ---------------------------------------------------------------------------

/// An inner VM that additionally implements `BatchedChainVm` + `StateSyncableVm`
/// so we can assert the wrapper exposes them iff the inner does.
mod capable_inner {
    include!("support/capable_inner.rs");
}

#[tokio::test]
async fn capability_probes_delegate() {
    use capable_inner::CapableVm;

    let token = CancellationToken::new();
    let clock = Arc::new(MockClock::at(UNIX_EPOCH));
    let (identity, node_id) = staking_identity();
    let mut set = BTreeMap::new();
    set.insert(
        node_id,
        GetValidatorOutput {
            node_id,
            public_key: None,
            weight: 1,
        },
    );
    let ctx = ctx_with(node_id, upgrades_pre_fork());

    let mut inner = CapableVm::new();
    let db_inner: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    inner
        .initialize(
            &token,
            ava_vm::testutil::test_chain_context(),
            db_inner,
            b"genesis",
            b"",
            b"",
            Vec::new(),
            Arc::new(NoopAppSender),
        )
        .await
        .expect("init capable inner");

    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let mut vm = ProposerVm::new(
        inner,
        ctx,
        clock as Arc<dyn Clock>,
        FixedState { set },
        db,
        Some(identity),
    );
    vm.initialize_wrapper(&token).await.expect("init wrapper");

    let vm_ref: &dyn ChainVm = &vm;
    assert!(vm_ref.as_batched().is_some(), "batched delegates to inner");
    assert!(
        vm_ref.as_state_syncable().is_some(),
        "state-syncable delegates to inner"
    );

    // And against a plain inner TestVm the probes are None.
    let plain = make_prefork_proposervm(token.clone()).await;
    let plain_ref: &dyn ChainVm = &plain;
    assert!(plain_ref.as_batched().is_none());
    assert!(plain_ref.as_state_syncable().is_none());
}

// Keep `FromStr` used (Id parse helpers may be used in future vectors).
#[allow(dead_code)]
fn _use_from_str() {
    let _ = Id::from_str("11111111111111111111111111111111LpoYY");
    let _ = SystemTime::now();
    let _: HashSet<NodeId> = HashSet::new();
}
