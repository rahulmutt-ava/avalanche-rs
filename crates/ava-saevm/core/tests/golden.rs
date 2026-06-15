// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden vectors for the SAE core: block hashes, settlement choices, and a
//! crash+restart recovery transcript (specs/11 §4.1/§1.2/§1.4, specs/02 §6).
//!
//! These freeze the *Rust pipeline's own* outputs as committed JSON so that any
//! later refactor that silently changes block hashing, the `settle()` driver's
//! `LastSettled` choice, or the `recover()` reconstruction is caught as a
//! regression. The vectors are computed by driving the live VM lifecycle once
//! (the fixtures here mirror `tests/recovery.rs`); the generator below
//! (`#[ignore]`d) prints them for re-freezing.
//!
//! # Provenance & Go parity
//!
//! The behavioral reference is the Go `vms/saevm` node — `blocks/block.go`
//! (block hash = `keccak256(RLP(header))`), `blocks/settlement.go` (the
//! `LastSettled` choice in increasing height on the gas-time clock), and
//! `sae/recovery.go` (rebuild A/E/S from disk). These vectors are
//! **self-consistent Rust-computed values**, not extracted from a running Go
//! node (that path is unavailable in this sandbox). **Exact Go/geth byte parity
//! is deferred to the M7.29 differential.** Each vector file + `MANIFEST.json`
//! records this provenance.

// Readable reference arithmetic + small-index casts in the fixture builders; the
// loop counters are tiny constants, so truncation cannot occur here.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use ava_evm_reth::{B256, Header, RethBlock, SealedBlock};
use ava_saevm_adaptor::{BlockProperties, ChainVm};
use ava_saevm_blocks::{Block, ExecutionArtefacts, WorstCaseBounds};
use ava_saevm_core::recovery::{RecoverySource, recover};
use ava_saevm_core::{BlockBuilderSeam, BuildError, ExecutorSeam, Vm};
use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_proxytime::Time;
use ava_saevm_types::ExecutionResults;
use ava_vm::components::gas::Price;

// ---------------------------------------------------------------------------
// Block fixtures (mirror tests/recovery.rs so the chain is deterministic).
// ---------------------------------------------------------------------------

fn eth_block(
    number: u64,
    timestamp: u64,
    parent_hash: B256,
    state_root: B256,
) -> SealedBlock<RethBlock> {
    let header = Header {
        parent_hash,
        number,
        timestamp,
        gas_limit: 8_000_000,
        gas_used: 21_000,
        base_fee_per_gas: Some(7),
        state_root,
        ..Header::default()
    };
    SealedBlock::seal_slow(RethBlock::uncle(header))
}

/// A genesis (synchronous, self-settling) SAE block at height 0.
fn genesis() -> Arc<Block> {
    let g =
        Arc::new(Block::new(eth_block(0, 0, B256::ZERO, B256::ZERO), None, None).expect("genesis"));
    g.mark_synchronous((
        ava_vm::components::gas::Gas(0),
        ava_saevm_gastime::GasPriceConfig::default(),
    ))
    .expect("mark synchronous");
    g
}

fn bounds() -> WorstCaseBounds {
    WorstCaseBounds {
        max_base_fee: Price(7),
        latest_end_time: GasTime::new(
            0,
            0,
            ava_vm::components::gas::Price(0),
            GasPriceConfig::default(),
        ),
        min_op_burner_balances: Vec::new(),
    }
}

/// Deterministic, height-derived execution results so a re-execution from disk
/// reproduces the exact same roots (invariant 7). The receipt root is also
/// height-derived (and distinct from the state root) so the settlement vector
/// pins both independently.
fn results_at(height: u64, exec_unix: u64) -> ExecutionResults {
    ExecutionResults {
        gas_time: Time::<u64>::new(exec_unix, 0, 1),
        base_fee: Price(1),
        receipt_root: B256::repeat_byte(u8::try_from((height % 251) + 1).unwrap_or(1)),
        post_state_root: B256::repeat_byte(u8::try_from(height % 251).unwrap_or(0)),
    }
}

// ---------------------------------------------------------------------------
// Fake builder + executor seams (live VM, mirror tests/recovery.rs).
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct FakeBuilder;

impl FakeBuilder {
    fn settled_root(parent: &Arc<Block>) -> B256 {
        parent
            .last_settled()
            .map_or(B256::ZERO, |s| s.post_execution_state_root())
    }

    fn assemble(parent: &Arc<Block>) -> Result<Arc<Block>, BuildError> {
        let height = parent.height() + 1;
        let timestamp = parent.build_time() + 1;
        let eth = eth_block(height, timestamp, parent.hash(), Self::settled_root(parent));
        let last_settled = parent.last_settled();
        let block = Block::new(eth, Some(Arc::clone(parent)), last_settled)
            .map_err(|e| BuildError::Builder(e.to_string()))?;
        let block = Arc::new(block);
        block.set_worst_case_bounds(bounds());
        Ok(block)
    }
}

impl BlockBuilderSeam for FakeBuilder {
    fn build_on(&self, parent: &Arc<Block>) -> Result<Arc<Block>, BuildError> {
        Self::assemble(parent)
    }

    fn rebuild(&self, parent: &Arc<Block>, _b: &Arc<Block>) -> Result<Arc<Block>, BuildError> {
        Self::assemble(parent)
    }
}

/// A controllable executor that also records what it executed into the shared
/// [`DiskState`], so the same durable artefacts survive a "restart".
struct FakeExecutor {
    disk: Arc<DiskState>,
    queue: Mutex<Vec<Arc<Block>>>,
}

impl FakeExecutor {
    fn new(disk: Arc<DiskState>) -> Self {
        Self {
            disk,
            queue: Mutex::new(Vec::new()),
        }
    }

    fn run_next(&self) {
        let next = {
            let q = self.queue.lock();
            q.iter().find(|b| !b.executed()).map(Arc::clone)
        };
        if let Some(b) = next {
            let results = results_at(b.height(), b.build_time());
            let artefacts = ExecutionArtefacts {
                interim_execution_time: results.gas_time.clone(),
                results: results.clone(),
            };
            b.mark_executed(artefacts, None).expect("mark executed");
            self.disk.record(&b, results);
        }
    }
}

impl ExecutorSeam for FakeExecutor {
    fn enqueue(&self, block: &Arc<Block>) -> Result<(), BuildError> {
        self.queue.lock().push(Arc::clone(block));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DiskState — the persistence + crash-point seam (mirror tests/recovery.rs).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct DiskState {
    canonical: Mutex<BTreeMap<u64, (SealedBlock<RethBlock>, ExecutionResults)>>,
}

impl DiskState {
    fn record(&self, block: &Arc<Block>, results: ExecutionResults) {
        self.canonical
            .lock()
            .insert(block.height(), (block.eth_block().clone(), results));
    }

    fn head(&self) -> u64 {
        self.canonical.lock().keys().copied().max().unwrap_or(0)
    }

    fn snapshot(&self, last_synchronous: Arc<Block>, commit_interval: u64) -> Snapshot {
        let canonical = self.canonical.lock().clone();
        Snapshot {
            last_synchronous,
            head: canonical.keys().copied().max().unwrap_or(0),
            canonical,
            commit_interval,
        }
    }
}

struct Snapshot {
    last_synchronous: Arc<Block>,
    head: u64,
    canonical: BTreeMap<u64, (SealedBlock<RethBlock>, ExecutionResults)>,
    commit_interval: u64,
}

impl RecoverySource for Snapshot {
    fn last_synchronous(&self) -> Arc<Block> {
        Arc::clone(&self.last_synchronous)
    }

    fn head_height(&self) -> u64 {
        self.head
    }

    fn last_committed_height(&self) -> u64 {
        if self.commit_interval == 0 {
            return self.head;
        }
        let rem = self.head % self.commit_interval;
        self.head.saturating_sub(rem)
    }

    fn canonical_eth_block(&self, height: u64) -> Option<SealedBlock<RethBlock>> {
        if height == self.last_synchronous.height() {
            return Some(self.last_synchronous.eth_block().clone());
        }
        self.canonical.get(&height).map(|(eth, _)| eth.clone())
    }

    fn execution_results(&self, height: u64) -> Option<ExecutionResults> {
        self.canonical.get(&height).map(|(_, r)| r.clone())
    }
}

// ---------------------------------------------------------------------------
// Live chain driver — the fixed chain the goldens freeze.
// ---------------------------------------------------------------------------

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000)
}

/// The length of the fixed golden chain (matches a recovery-transcript snapshot
/// boundary so settlement actually advances S below the tip).
const CHAIN_LEN: u64 = 8;

/// Drives the live VM through `CHAIN_LEN` blocks and returns the disk + genesis
/// + the accepted-block chain (genesis at index 0) + the frontier.
struct LiveChain {
    disk: Arc<DiskState>,
    genesis: Arc<Block>,
    chain: Vec<Arc<Block>>,
    settled_h: u64,
    executed_h: u64,
    accepted_h: u64,
    settled_state_root: B256,
    settled_receipt_root: B256,
}

async fn build_live_chain(n: u64) -> LiveChain {
    let disk = Arc::new(DiskState::default());
    let g = genesis();
    let exec = Arc::new(FakeExecutor::new(Arc::clone(&disk)));
    let vm = Arc::new(Vm::new(&g, FakeBuilder, Arc::clone(&exec), now));

    let mut chain: Vec<Arc<Block>> = vec![Arc::clone(&g)];
    let mut head: Arc<Block> = Arc::clone(&g);
    for _ in 0..n {
        let built = vm.build_block(None).await.expect("build");
        vm.verify_block(None, &built).await.expect("verify");
        vm.accept_block(&built).await.expect("accept");
        exec.run_next();
        vm.set_preference(built.id(), None).await.expect("pref");
        head = Arc::clone(built.block());
        chain.push(Arc::clone(&head));
    }

    let f = vm.frontier();
    let settled = f.last_settled();
    let settled_receipt_root = settled
        .execution_results()
        .map_or(B256::ZERO, |r| r.receipt_root);
    LiveChain {
        disk,
        genesis: g,
        settled_h: settled.height(),
        executed_h: head.height(),
        accepted_h: f.last_accepted().height(),
        settled_state_root: settled.post_execution_state_root(),
        settled_receipt_root,
        chain,
    }
}

// ---------------------------------------------------------------------------
// Vector path resolution (workspace root: crates/ava-saevm/core/ -> 4 up).
// ---------------------------------------------------------------------------

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/vectors/saevm")
}

fn read_vector(rel: &str) -> serde_json::Value {
    let path = vectors_dir().join(rel);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read golden vector {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse golden json {rel}: {e}"))
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

// ---------------------------------------------------------------------------
// Golden tests.
// ---------------------------------------------------------------------------

mod golden {
    use super::{B256, CHAIN_LEN, build_live_chain, hex0x, read_vector, recover};
    use std::sync::Arc;

    /// Block hashes for the fixed chain equal the committed vector. This is the
    /// core-side companion to the blocks-crate `block_hash.json` golden: it pins
    /// the hash of every block produced by the live builder (not a single hand
    /// built header), so any layout/linkage drift in the build path is caught.
    #[tokio::test]
    async fn sae_block_hash() {
        let live = build_live_chain(CHAIN_LEN).await;
        let v = read_vector("blocks/chain_block_hashes.json");

        let want: Vec<String> = v["block_hashes"]
            .as_array()
            .expect("block_hashes array")
            .iter()
            .map(|h| h.as_str().expect("hash string").to_string())
            .collect();

        let got: Vec<String> = live
            .chain
            .iter()
            .map(|b| hex0x(b.hash().as_slice()))
            .collect();

        assert_eq!(
            got.len(),
            want.len(),
            "chain length drifted from committed golden",
        );
        assert_eq!(got, want, "SAE block hashes drifted from committed golden");
    }

    /// The settlement choice for the fixed chain — `(LastSettled height/hash,
    /// settled receipt root, settled state root)` — equals the committed vector.
    /// This freezes the `settle()` driver's `LastSettled` choice (increasing
    /// height on the gas-time clock, specs/11 §1.2).
    #[tokio::test]
    async fn settlement_vectors() {
        let live = build_live_chain(CHAIN_LEN).await;
        let v = read_vector("settlement/settlement_choice.json");

        assert_eq!(
            live.accepted_h,
            v["last_accepted_height"].as_u64().expect("la height"),
            "LastAccepted height drifted",
        );
        assert_eq!(
            live.executed_h,
            v["last_executed_height"].as_u64().expect("le height"),
            "LastExecuted height drifted",
        );
        assert_eq!(
            live.settled_h,
            v["last_settled_height"].as_u64().expect("ls height"),
            "LastSettled height drifted from committed golden",
        );

        // The settled block's hash + roots.
        let settled = &live.chain[live.settled_h as usize];
        assert_eq!(
            hex0x(settled.hash().as_slice()),
            v["last_settled_hash"].as_str().expect("ls hash"),
            "LastSettled block hash drifted",
        );
        assert_eq!(
            hex0x(live.settled_state_root.as_slice()),
            v["settled_state_root"]
                .as_str()
                .expect("settled state root"),
            "settled state root drifted",
        );
        assert_eq!(
            hex0x(live.settled_receipt_root.as_slice()),
            v["settled_receipt_root"]
                .as_str()
                .expect("settled receipt root"),
            "settled receipt root drifted",
        );
    }

    /// A recorded crash+restart transcript reconstructs the recorded A/E/S
    /// frontiers + roots. We drive the live chain, snapshot the durable disk at
    /// a fixed commit interval (the crash point), drop the live VM, then
    /// `recover()` and assert the reconstructed frontiers + post-state roots
    /// equal the committed transcript (specs/11 §1.4, §10 invariant 7).
    #[tokio::test]
    async fn recovery_transcript() {
        let live = build_live_chain(CHAIN_LEN).await;
        let v = read_vector("recovery/recovery_transcript.json");

        let commit_interval = v["commit_interval"].as_u64().expect("commit_interval");
        let snap = live
            .disk
            .snapshot(Arc::clone(&live.genesis), commit_interval);
        let recovered = recover(&snap).await.expect("recover");
        let f = &recovered.frontier;

        // Reconstructed frontier heights match the recorded transcript AND the
        // live run (cross-check).
        assert_eq!(
            f.last_accepted().height(),
            v["recovered_accepted_height"].as_u64().expect("ra height"),
            "recovered A height drifted from transcript",
        );
        assert_eq!(
            f.last_executed().expect("E").height(),
            v["recovered_executed_height"].as_u64().expect("re height"),
            "recovered E height drifted from transcript",
        );
        assert_eq!(
            f.last_settled().height(),
            v["recovered_settled_height"].as_u64().expect("rs height"),
            "recovered S height drifted from transcript",
        );
        assert_eq!(f.last_accepted().height(), live.accepted_h);
        assert_eq!(f.last_executed().expect("E").height(), live.executed_h);
        assert_eq!(f.last_settled().height(), live.settled_h);

        // Reconstructed roots match the recorded transcript.
        let exec_root: B256 = f.last_executed().expect("E").post_execution_state_root();
        let settled_root: B256 = f.last_settled().post_execution_state_root();
        assert_eq!(
            hex0x(exec_root.as_slice()),
            v["recovered_executed_state_root"]
                .as_str()
                .expect("re root"),
            "recovered executed state root drifted from transcript",
        );
        assert_eq!(
            hex0x(settled_root.as_slice()),
            v["recovered_settled_state_root"].as_str().expect("rs root"),
            "recovered settled state root drifted from transcript",
        );

        // Head cross-check (A == E == head; recovery re-executes to the tip).
        assert_eq!(f.last_accepted().height(), live.disk.head());
        assert!(f.heights_ordered(), "S <= E <= A after recovery");
    }
}

// ---------------------------------------------------------------------------
// Vector generator — run with `--ignored` to print the freezable values, e.g.
//   cargo test -p ava-saevm-core --test golden -- --ignored --nocapture
// then paste the printed JSON into tests/vectors/saevm/{...}.json. Kept in-tree
// (clearly marked) so re-freezing after an intentional change is trivial.
// ---------------------------------------------------------------------------

#[ignore = "vector generator; run with --ignored --nocapture to re-freeze"]
#[tokio::test]
async fn generate_vectors() {
    let live = build_live_chain(CHAIN_LEN).await;

    println!("=== blocks/chain_block_hashes.json ===");
    let hashes: Vec<String> = live
        .chain
        .iter()
        .map(|b| hex0x(b.hash().as_slice()))
        .collect();
    println!("{}", serde_json::to_string_pretty(&hashes).unwrap());

    println!("=== settlement/settlement_choice.json ===");
    println!("last_accepted_height = {}", live.accepted_h);
    println!("last_executed_height = {}", live.executed_h);
    println!("last_settled_height  = {}", live.settled_h);
    let settled = &live.chain[live.settled_h as usize];
    println!(
        "last_settled_hash    = {}",
        hex0x(settled.hash().as_slice())
    );
    println!(
        "settled_state_root   = {}",
        hex0x(live.settled_state_root.as_slice())
    );
    println!(
        "settled_receipt_root = {}",
        hex0x(live.settled_receipt_root.as_slice())
    );

    println!("=== recovery/recovery_transcript.json (commit_interval=16) ===");
    let snap = live.disk.snapshot(Arc::clone(&live.genesis), 16);
    let recovered = recover(&snap).await.expect("recover");
    let f = &recovered.frontier;
    println!(
        "recovered_accepted_height    = {}",
        f.last_accepted().height()
    );
    println!(
        "recovered_executed_height    = {}",
        f.last_executed().expect("E").height()
    );
    println!(
        "recovered_settled_height     = {}",
        f.last_settled().height()
    );
    println!(
        "recovered_executed_state_root = {}",
        hex0x(
            f.last_executed()
                .expect("E")
                .post_execution_state_root()
                .as_slice()
        )
    );
    println!(
        "recovered_settled_state_root  = {}",
        hex0x(f.last_settled().post_execution_state_root().as_slice())
    );
}
