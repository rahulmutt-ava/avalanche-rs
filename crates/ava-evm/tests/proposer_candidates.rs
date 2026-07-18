// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.15 Task 6 — the offline Go-oracle verdict leg (recorded differential):
//! REAL coreth code judges Rust-**built** C-Chain block bytes.
//!
//! This is a two-step recorded oracle (specs/02 §11.1), the same shape as the
//! M7.29 recovery differential (`tests/differential/go-oracle/
//! recovery_vector_emitter_test.go` + `tests/vectors/saevm/recovery_differential/`
//! + `sae_recovery.rs`), but here the emitter runs on the **Rust** side and the
//!   judge on the **Go** side (the M7.29 shape is the reverse):
//!
//! 1. [`emit_proposer_candidates`] (env-gated, operator step): builds the
//!    "honest" candidate — a real block the Task 2-5 builder produces on the
//!    committed `local.json` C-Chain genesis (`vectors/cchain/genesis/local.json`,
//!    already the Go-oracle genesis fixture `cancun_clamp.rs` boots — LOCAL_ID's
//!    schedule activates every fork through Granite at `InitiallyActiveTime`,
//!    matching Go's `upgradetest.Granite`), carrying one signed EVM tx — plus
//!    five adversarial mutations of it (decode → mutate → re-encode, the
//!    `cancun_clamp.rs:57-96` pattern). Writes `<name>.rlp.hex` + a copy of the
//!    genesis JSON into the output directory.
//! 2. The companion Go judge (`tests/differential/go-oracle/
//!    rust_built_block_verdict_test.go`, dropped into `~/avalanchego/graft/
//!    coreth/plugin/evm/` to run) boots a real coreth test VM over the SAME
//!    genesis JSON, `ParseBlock`s + `Verify`s each candidate, and writes
//!    `verdicts.json`.
//! 3. [`proposer_verdicts_hold`] (per-PR): loads the committed `verdicts.json`,
//!    asserts the honest verdict is `accepted == true`, and for every
//!    adversarial candidate asserts BOTH the recorded Go verdict is a rejection
//!    whose error names the expected sentinel AND that Rust's own
//!    `EvmVm::parse_block` → `Block::verify` entry (the same one the `ChainVm`
//!    adapter drives) rejects the identical bytes with the matching Rust
//!    sentinel — i.e. Go and Rust reject the SAME candidate for the SAME
//!    reason.
//!
//! # Re-recording (operator, live mode)
//!
//! ```sh
//! ./scripts/check_oracle_binary.sh   # must print OK before recording
//! EMIT_PROPOSER_CANDIDATES=$PWD/crates/ava-evm/tests/vectors/proposer_verdict \
//!   cargo test -p ava-evm --test proposer_candidates -- --exact emit_proposer_candidates
//! cp tests/differential/go-oracle/rust_built_block_verdict_test.go \
//!    ~/avalanchego/graft/coreth/plugin/evm/
//! cd ~/avalanchego && AVALANCHEGO_COMMIT=$(git rev-parse HEAD) \
//! RUST_BLOCK_VERDICT_DIR=$OLDPWD/crates/ava-evm/tests/vectors/proposer_verdict \
//!   go test -run TestRustBuiltBlockVerdicts ./graft/coreth/plugin/evm/ && \
//!   rm graft/coreth/plugin/evm/rust_built_block_verdict_test.go
//! ```
//!
//! Without `EMIT_PROPOSER_CANDIDATES` set, [`emit_proposer_candidates`] is a
//! no-op, so the emitter never runs during a normal `cargo test`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use ava_crypto::secp256k1::PrivateKey;
use ava_database::{DynDatabase, MemDb};
use ava_evm::atomic::mempool::AtomicMempool;
use ava_evm::block::{AvaBlockParts, EvmBlockContext, assemble_ava_block, decode_ava_evm_block};
use ava_evm::builder::BlockBuilderDriver;
use ava_evm::canonical::CanonicalStore;
use ava_evm::chainspec::{AvaChainSpec, CChainGenesis};
use ava_evm::evmconfig::{AvaEvmConfig, AvaNextBlockCtx};
use ava_evm::feerules::parent_fee_state_of;
use ava_evm::state::FirewoodStateProvider;
use ava_evm::vm::EvmVm;
use ava_evm_reth::{
    Address, B256, Chain, EvmSignature, SignableTransaction, SignerRecoverable, TransactionSigned,
    TxKind, TxLegacy, U256,
};
use ava_types::constants::LOCAL_ID;
use ava_types::id::Id;
use ava_vm::block::ChainVm;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

/// The well-known "ewoq" pre-funded private key on `local` networks (the same
/// constant `tests/differential/src/livenet.rs::EWOQ_KEY_HEX` carries).
///
/// Address: `0x8db97C7cEcE249c2b98bDC0226Cc4C2A57BF52FC` — the sole `alloc`
/// entry in `vectors/cchain/genesis/local.json`.
const EWOQ_KEY_HEX: &str = "56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027";

/// A gas price comfortably above the AP3 genesis base fee (225 gwei) so the
/// honest candidate's tx is never underpriced.
const HONEST_TX_GAS_PRICE: u128 = 300_000_000_000;

/// The committed C-Chain local genesis (also `cancun_clamp.rs`'s Go-oracle
/// fixture) — reused verbatim so both the Rust builder and the Go judge boot
/// from byte-identical genesis JSON.
fn local_genesis_json() -> &'static str {
    include_str!("vectors/cchain/genesis/local.json")
}

/// The committed corpus directory (workspace-rooted).
fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/proposer_verdict")
}

/// A named header-corrupting mutation applied to the honest candidate's
/// decoded parts.
type Mutation = (&'static str, fn(&mut AvaBlockParts));

/// The five adversarial mutations (brief-mandated set): each takes the honest
/// candidate's decoded parts and corrupts exactly one `syntacticVerify` check,
/// leaving every earlier-checked field untouched so Go's (and Rust's) first
/// rejection is the intended one (`wrapped_block.go:398-527` / `block.rs`
/// `syntactic_verify` — both walk the checks in the same order).
const MUTATIONS: [Mutation; 5] = [
    ("zero_difficulty", |p| p.header.difficulty = U256::ZERO),
    ("missing_cancun_tail", |p| {
        p.header.parent_beacon_root = None;
        p.header.blob_gas_used = None;
        p.header.excess_blob_gas = None;
    }),
    ("wrong_tx_root", |p| {
        p.header.tx_root = B256::repeat_byte(0x11)
    }),
    ("bad_coinbase", |p| {
        p.header.coinbase = Address::repeat_byte(0x33)
    }),
    ("nonzero_nonce", |p| {
        p.header.nonce = [0, 0, 0, 0, 0, 0, 0, 1]
    }),
];

/// name -> (Go rejection substring, Rust rejection substring). Most pairs are
/// textually identical — Rust's `error.rs` sentinel messages are written to
/// mirror coreth's verbatim (`wrapped_block.go:398-527`) — but they are
/// checked independently against each side's own error text, so a future
/// divergence in either message would be caught rather than silently matching
/// a shared constant.
///
/// `missing_cancun_tail` is the one asymmetric pair: omitting
/// `parentBeaconRoot`/blob fields changes the header's RLP shape, so Go's
/// decoder rejects it structurally (`rlp: input string too short for
/// common.Hash, decoding into ...ParentBeaconRoot`) before ever reaching
/// `wrapped_block.go`'s semantic "missing parentBeaconRoot" check, whereas
/// Rust's decoder tolerates the shorter shape and rejects it later, at the
/// semantic check, with that exact sentinel. Both are correct rejections of
/// the same malformed candidate at different (equally valid) layers — the
/// same asymmetry Task 3 documented for non-empty uncle lists.
const REJECTION_CLASSES: [(&str, &str, &str); 5] = [
    (
        "zero_difficulty",
        "invalid difficulty",
        "invalid difficulty",
    ),
    (
        "missing_cancun_tail",
        "ParentBeaconRoot",
        "missing parentBeaconRoot",
    ),
    ("wrong_tx_root", "invalid txs hash", "invalid txs hash"),
    ("bad_coinbase", "invalid coinbase", "invalid coinbase"),
    ("nonzero_nonce", "invalid nonce", "invalid nonce"),
];

/// Decodes `base`, applies `mutate` to its parts, and re-assembles fresh
/// self-consistent wire bytes (the block hash is recomputed over the mutated
/// header) — the `cancun_clamp.rs:57-96` mutate+re-encode pattern, without the
/// proposervm-container unwrap step (our candidates are raw coreth block
/// bytes, not proposervm-wrapped).
fn mutate_candidate(spec: &AvaChainSpec, base: &[u8], mutate: fn(&mut AvaBlockParts)) -> Vec<u8> {
    let block = decode_ava_evm_block(base, spec).expect("decode honest candidate for mutation");
    let mut parts = block.into_parts();
    mutate(&mut parts);
    assemble_ava_block(parts, spec)
        .expect("assemble mutated candidate")
        .encoded_bytes()
        .to_vec()
}

/// Env-gated candidate writer (operator step). Builds the honest candidate —
/// a real block the Task 2-5 [`BlockBuilderDriver`] produces on the committed
/// local genesis, carrying one signed EVM tx from the pre-funded "ewoq"
/// key — asserts it passes Rust's own full `syntactic_verify` (never freeze a
/// corpus whose honest candidate Rust itself would reject), then emits it plus
/// the five [`MUTATIONS`] as `<name>.rlp.hex` alongside a copy of the genesis
/// JSON, into `$EMIT_PROPOSER_CANDIDATES`.
#[test]
fn emit_proposer_candidates() {
    let Ok(out_dir) = std::env::var("EMIT_PROPOSER_CANDIDATES") else {
        return;
    };
    let out_dir = PathBuf::from(out_dir);
    std::fs::create_dir_all(&out_dir)
        .unwrap_or_else(|e| panic!("create {}: {e}", out_dir.display()));

    let genesis_json = local_genesis_json();
    let genesis = CChainGenesis::parse(genesis_json).expect("parse local genesis");
    let chain_spec = AvaChainSpec::c_chain(LOCAL_ID, Chain::from_id(genesis.chain_id()));
    let (genesis_bundle, genesis_bytecode) = genesis.genesis_alloc(chain_spec.network_upgrades());

    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open firewood");
    for (code_hash, code) in &genesis_bytecode {
        provider
            .bytecode_store()
            .put(code_hash.as_slice(), code)
            .expect("seed genesis bytecode");
    }
    let genesis_root = provider
        .propose_from_bundle(&genesis_bundle)
        .expect("propose genesis alloc");
    provider.commit(genesis_root).expect("commit genesis alloc");

    let genesis_header = genesis.genesis_header(genesis_root, chain_spec.network_upgrades());

    let config = AvaEvmConfig::new(chain_spec.clone());
    let canonical = Arc::new(CanonicalStore::new(Arc::new(MemDb::new())));
    let txpool = Arc::new(Mutex::new(AtomicMempool::new(64, Id::EMPTY)));
    let driver = BlockBuilderDriver::new(config.clone(), Arc::clone(&provider), txpool);

    // One EVM tx: a self-transfer from the pre-funded "ewoq" key, signed
    // EIP-155 over the genesis chain id (`sign_legacy` pattern, `evm_factory.rs`).
    let ewoq_key = PrivateKey::from_bytes(&hex::decode(EWOQ_KEY_HEX).expect("ewoq key hex"))
        .expect("ewoq key");
    let ewoq_addr = Address::from(ewoq_key.public_key().eth_address());
    let tx = TxLegacy {
        chain_id: Some(genesis.chain_id()),
        nonce: 0,
        gas_price: HONEST_TX_GAS_PRICE,
        gas_limit: 21_000,
        to: TxKind::Call(ewoq_addr),
        value: U256::from(1u64),
        input: Default::default(),
    };
    let sig_hash = tx.signature_hash();
    let rsv = ewoq_key.sign_hash(&sig_hash.0).expect("sign honest tx");
    let r = U256::from_be_slice(&rsv[..32]);
    let s = U256::from_be_slice(&rsv[32..64]);
    let sig = EvmSignature::new(r, s, rsv[64] == 1);
    let signed = TransactionSigned::Legacy(tx.into_signed(sig));
    let evm_tx = signed
        .try_into_recovered()
        .expect("recover honest tx sender");

    // Granite ACP-226 enforces a minimum inter-block delay derived from the
    // parent's `MinDelayExcess` (coreth `customheader/time.go:103-115`
    // `VerifyTime`); the local genesis carries `INITIAL_DELAY_EXCESS`, whose
    // `.delay()` is exactly 2000ms (`acp226.rs` golden table), so the child
    // must land >= 2 seconds after the genesis timestamp or Go's `Verify`
    // rejects it with "minimum block delay not met" — a real ACP-226 floor,
    // not a builder defect (Rust does not yet port this check itself).
    const MIN_BLOCK_DELAY_SECS: u64 = 2;
    let parent_time = genesis_header.time;
    let next_time = parent_time.saturating_add(MIN_BLOCK_DELAY_SECS);
    let ctx = AvaNextBlockCtx {
        timestamp: next_time,
        timestamp_ms: next_time.saturating_mul(1000),
        suggested_fee_recipient: Address::ZERO,
        parent_fee_state: parent_fee_state_of(config.chain_spec(), &genesis_header)
            .expect("parent fee state"),
        ..AvaNextBlockCtx::with_atomic_gas_limit(100_000)
    };

    let built = driver
        .build_on(&genesis_header, genesis_root, &ctx, vec![evm_tx])
        .expect("build honest candidate");
    assert_eq!(
        built.transactions().len(),
        1,
        "honest candidate must carry the one EVM tx"
    );

    // Self-check (never commit a corpus whose honest verdict is a rejection):
    // the honest candidate must pass Rust's own full `syntactic_verify` +
    // execution through the SAME `EvmBlock::verify` entry the `ChainVm`
    // adapter drives.
    let built_root = *built.header_state_root();
    provider.discard(built_root);
    let block_ctx = EvmBlockContext::new(Arc::clone(&provider), config, canonical);
    built
        .verify(&block_ctx, genesis_root)
        .expect("honest candidate must pass Rust's own full syntactic_verify");

    let honest_bytes = built.encoded_bytes().to_vec();
    std::fs::write(out_dir.join("honest.rlp.hex"), hex::encode(&honest_bytes))
        .expect("write honest.rlp.hex");

    for (name, mutate) in MUTATIONS {
        let bytes = mutate_candidate(&chain_spec, &honest_bytes, mutate);
        std::fs::write(out_dir.join(format!("{name}.rlp.hex")), hex::encode(&bytes))
            .unwrap_or_else(|e| panic!("write {name}.rlp.hex: {e}"));
    }
    std::fs::write(out_dir.join("genesis.json"), genesis_json).expect("write genesis.json");

    eprintln!(
        "wrote {} proposer-verdict candidates to {}",
        1 + MUTATIONS.len(),
        out_dir.display()
    );
}

#[derive(serde::Deserialize)]
struct VerdictEntry {
    name: String,
    accepted: bool,
    #[serde(default)]
    error: String,
}

#[derive(serde::Deserialize)]
struct VerdictsFile {
    verdicts: Vec<VerdictEntry>,
}

/// Drives Rust's own `EvmVm::parse_block` -> `Block::verify` entry (the same
/// one the `ChainVm` adapter drives) over `bytes`, booted on `genesis_json`.
/// Mirrors `cancun_clamp.rs`'s `parse_and_verify` helper.
async fn parse_and_verify(genesis_json: &str, bytes: &[u8]) -> Result<(), String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vm, _genesis_id) = EvmVm::from_genesis(LOCAL_ID, dir.path(), genesis_json.as_bytes())
        .expect("EvmVm::from_genesis over the committed local genesis");
    let token = CancellationToken::new();
    let blk = vm
        .parse_block(&token, bytes)
        .await
        .map_err(|e| e.to_string())?;
    blk.verify(&token).await.map_err(|e| e.to_string())
}

/// Per-PR reader (TDD RED until the recording step writes `verdicts.json`):
/// loads the committed Go-oracle verdicts, asserts the honest candidate was
/// **accepted**, and for every adversarial candidate asserts BOTH the
/// recorded Go verdict is a rejection naming the expected sentinel AND that
/// Rust's own `parse_and_verify` rejects the identical bytes with the matching
/// sentinel — Go and Rust reject the SAME candidate for the SAME reason.
#[tokio::test]
async fn proposer_verdicts_hold() {
    let dir = corpus_dir();

    let verdicts_path = dir.join("verdicts.json");
    let raw = std::fs::read_to_string(&verdicts_path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e} (run the Task 6 recording step first: \
             EMIT_PROPOSER_CANDIDATES=... cargo test -p ava-evm --test proposer_candidates \
             -- --exact emit_proposer_candidates, then the Go judge)",
            verdicts_path.display()
        )
    });
    let file: VerdictsFile = serde_json::from_str(&raw).expect("parse verdicts.json");
    let by_name: BTreeMap<&str, &VerdictEntry> =
        file.verdicts.iter().map(|v| (v.name.as_str(), v)).collect();

    let genesis_json =
        std::fs::read_to_string(dir.join("genesis.json")).expect("read committed genesis.json");

    // The honest candidate: Go must ACCEPT it, and so must Rust's own entry.
    let honest = by_name
        .get("honest")
        .expect("verdicts.json carries a \"honest\" entry");
    assert!(
        honest.accepted,
        "coreth must ACCEPT the honest Rust-built block, got error: {:?}",
        honest.error
    );
    let honest_hex =
        std::fs::read_to_string(dir.join("honest.rlp.hex")).expect("read honest.rlp.hex");
    let honest_bytes = hex::decode(honest_hex.trim()).expect("decode honest.rlp.hex");
    parse_and_verify(&genesis_json, &honest_bytes)
        .await
        .expect("Rust must also accept the honest candidate via the ChainVm entry");

    // Every adversarial candidate: Go rejects with the expected sentinel, and
    // so does Rust, driven independently over the identical bytes.
    for (name, go_substr, rust_substr) in REJECTION_CLASSES {
        let go_verdict = by_name
            .get(name)
            .unwrap_or_else(|| panic!("verdicts.json is missing the {name:?} entry"));
        assert!(
            !go_verdict.accepted,
            "coreth must REJECT {name}, but it was accepted"
        );
        assert!(
            go_verdict.error.contains(go_substr),
            "{name}: Go error {:?} does not contain {go_substr:?}",
            go_verdict.error
        );

        let hex_str = std::fs::read_to_string(dir.join(format!("{name}.rlp.hex")))
            .unwrap_or_else(|e| panic!("read {name}.rlp.hex: {e}"));
        let bytes = hex::decode(hex_str.trim()).unwrap_or_else(|e| panic!("decode {name}: {e}"));

        let rust_err = parse_and_verify(&genesis_json, &bytes)
            .await
            .expect_err(&format!("Rust must also reject {name}"));
        assert!(
            rust_err.contains(rust_substr),
            "{name}: Rust error {rust_err:?} does not contain {rust_substr:?}"
        );
    }
}
