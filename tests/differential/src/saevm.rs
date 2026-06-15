// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! SAE crash+restart recovery collector for the M7.29 differential
//! (`differential::sae_recovery`, specs/11 §1.4/§10 invariant 7, specs/27 §9).
//!
//! The cross-implementation oracle is the **live Go `vms/saevm` node**: a Go
//! vector-emitter (`tests/differential/go-oracle/`) drives the real Go SAE VM
//! through a scripted block stream, crashes (snapshots the durable DB) at a
//! chosen crash point, restarts via Go `recover()`, and records — per height —
//! the canonical block **wire bytes** plus the committed `ExecutionResults`
//! (gas-time, base fee, receipt/state roots), and the source + recovered A/E/S
//! frontier observations, as JSON under
//! `tests/vectors/saevm/recovery_differential/`.
//!
//! This collector replays that corpus against the **real Rust SAE recovery
//! pipeline** ([`ava_saevm_core::recover`]):
//!
//! 1. [`parse_block`] each Go-emitted wire-byte blob → a byte-identical Rust
//!    [`Block`] (hash = `keccak256(RLP(header))`, parity **by construction** —
//!    the Rust reth/alloy decoder and the Go geth encoder share the wire form).
//! 2. populate a `VectorRecoverySource` with the parsed canonical bodies + the
//!    Go-emitted [`ExecutionResults`] + the crash-point commit interval.
//! 3. run [`recover`] and collect a normalized [`Observation`]: the
//!    reconstructed A/E/S frontier heights, the `LastSettled` block hash, and the
//!    settled/executed post-state roots.
//!
//! The genuine differential is on the **decisions Rust computes**, not on bytes
//! fed straight through: the **settlement choice** (which height becomes
//! `LastSettled`) is recomputed by the Rust `last_to_settle_at` walk over the
//! Go-emitted gas-times + parsed build-times, and the **block hashes** are
//! recomputed by the Rust decoder. Real-EVM **state roots are the Go-emitted
//! values fed into the source** (so the firewood v0.5.0-vs-v0.6.0 hash question
//! is moot here — see the M7.29 status note); they round-trip through recovery
//! unchanged, which still verifies that recovery restores the *same* root it was
//! given (invariant 7).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_evm_reth::{B256, RethBlock, SealedBlock};
use ava_saevm_blocks::{Block, ExecutionArtefacts, parse_block};
use ava_saevm_core::recovery::{RecoverError, RecoverySource, recover};
use ava_saevm_core::{Frontier, SettleError, settle};
use ava_saevm_proxytime::Time;
use ava_saevm_types::ExecutionResults;
use ava_vm::components::gas::Price;

use crate::observation::Observation;

/// A wall-clock instant safely in the future of every emitted block timestamp,
/// used for the `parse_block` future-block bound (the corpus blocks sit near the
/// Unix epoch + a few seconds, so any large constant clears the bound).
fn parse_now() -> SystemTime {
    UNIX_EPOCH
        .checked_add(Duration::from_secs(1_000_000_000))
        .unwrap_or(UNIX_EPOCH)
}

/// Failure replaying a recovery vector against the Rust pipeline.
#[derive(Debug, thiserror::Error)]
pub enum VectorError {
    /// The committed JSON corpus could not be parsed.
    #[error("parsing recovery vector JSON: {0}")]
    Json(String),
    /// A hex field (`0x…`) in the corpus was malformed.
    #[error("decoding hex field {field}: {source}")]
    Hex {
        /// The offending JSON field name.
        field: String,
        /// The underlying hex error.
        source: hex::FromHexError,
    },
    /// A required field was absent from the corpus.
    #[error("recovery vector missing required field {0}")]
    MissingField(&'static str),
    /// Decoding a Go-emitted block's wire bytes failed.
    #[error("parsing block at height {height}: {source}")]
    ParseBlock {
        /// The block height that failed to parse.
        height: u64,
        /// The underlying parse error.
        source: ava_saevm_blocks::ParseError,
    },
    /// The Rust `recover()` itself failed.
    #[error("rust recover() failed: {0}")]
    Recover(#[from] RecoverError),
    /// Marking the parsed genesis synchronous failed.
    #[error("marking genesis synchronous: {0}")]
    Genesis(ava_saevm_blocks::Error),
    /// Building / executing a streamed block against the Rust frontier failed.
    #[error("driving streamed block at height {height}: {source}")]
    Stream {
        /// The barrier height that failed to drive.
        height: u64,
        /// The underlying block-lifecycle error.
        source: ava_saevm_blocks::Error,
    },
    /// The Rust `settle()` walk failed for a streamed barrier.
    #[error("settle() at height {height}: {source}")]
    Settle {
        /// The barrier height whose settlement failed.
        height: u64,
        /// The underlying settle error.
        source: SettleError,
    },
}

/// Decode a `0x`-prefixed (or bare) hex string into a fixed-width [`B256`].
fn parse_b256(field: &'static str, s: &str) -> Result<B256, VectorError> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(trimmed).map_err(|source| VectorError::Hex {
        field: field.to_string(),
        source,
    })?;
    if bytes.len() != 32 {
        return Err(VectorError::Hex {
            field: field.to_string(),
            source: hex::FromHexError::InvalidStringLength,
        });
    }
    Ok(B256::from_slice(&bytes))
}

/// Decode a `0x`-prefixed (or bare) hex byte blob.
fn parse_hex(field: &'static str, s: &str) -> Result<Vec<u8>, VectorError> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    hex::decode(trimmed).map_err(|source| VectorError::Hex {
        field: field.to_string(),
        source,
    })
}

fn str_field<'a>(v: &'a serde_json::Value, key: &'static str) -> Result<&'a str, VectorError> {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or(VectorError::MissingField(key))
}

fn u64_field(v: &serde_json::Value, key: &'static str) -> Result<u64, VectorError> {
    v.get(key)
        .and_then(serde_json::Value::as_u64)
        .ok_or(VectorError::MissingField(key))
}

/// A normalized A/E/S frontier observation, parsed from the Go corpus or
/// produced by replaying the Rust recovery. The cross-implementation comparison
/// is value-equality of two of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontierObservation {
    /// `LastAccepted` height.
    pub accepted_height: u64,
    /// `LastExecuted` height.
    pub executed_height: u64,
    /// `LastSettled` height.
    pub settled_height: u64,
    /// `LastSettled` block hash (`0x…`).
    pub settled_hash: String,
    /// Settled block post-execution state root (`0x…`).
    pub settled_state_root: String,
    /// Executed (head) block post-execution state root (`0x…`).
    pub executed_state_root: String,
}

impl FrontierObservation {
    /// Parse a `{accepted,executed,settled}_height + …_hash/root` object.
    fn from_json(v: &serde_json::Value) -> Result<Self, VectorError> {
        Ok(Self {
            accepted_height: u64_field(v, "accepted_height")?,
            executed_height: u64_field(v, "executed_height")?,
            settled_height: u64_field(v, "settled_height")?,
            settled_hash: str_field(v, "settled_hash")?.to_ascii_lowercase(),
            settled_state_root: str_field(v, "settled_state_root")?.to_ascii_lowercase(),
            executed_state_root: str_field(v, "executed_state_root")?.to_ascii_lowercase(),
        })
    }

    /// Flatten into the harness-wide [`Observation`] (sorted key/value strings).
    #[must_use]
    pub fn to_observation(&self) -> Observation {
        Observation {
            fields: vec![
                (
                    "accepted_height".to_string(),
                    self.accepted_height.to_string(),
                ),
                (
                    "executed_height".to_string(),
                    self.executed_height.to_string(),
                ),
                (
                    "settled_height".to_string(),
                    self.settled_height.to_string(),
                ),
                ("settled_hash".to_string(), self.settled_hash.clone()),
                (
                    "settled_state_root".to_string(),
                    self.settled_state_root.clone(),
                ),
                (
                    "executed_state_root".to_string(),
                    self.executed_state_root.clone(),
                ),
            ],
        }
        .normalized()
    }
}

/// A `RecoverySource` populated entirely from the Go-oracle corpus: the parsed
/// canonical eth bodies (height-indexed) + the Go-emitted committed
/// [`ExecutionResults`] + the crash-point commit interval. Mirrors the in-memory
/// `Snapshot` the core recovery tests use, but every input originates from the
/// live Go node.
struct VectorRecoverySource {
    last_synchronous: Arc<Block>,
    head: u64,
    commit_interval: u64,
    canonical: BTreeMap<u64, (SealedBlock<RethBlock>, ExecutionResults)>,
}

impl RecoverySource for VectorRecoverySource {
    fn last_synchronous(&self) -> Arc<Block> {
        Arc::clone(&self.last_synchronous)
    }

    fn head_height(&self) -> u64 {
        self.head
    }

    fn last_committed_height(&self) -> u64 {
        // Mirrors saedb::LastCommittedTrieDBHeight / the Go oracle's crash point:
        // round the head DOWN to the last commit-interval boundary.
        if self.commit_interval == 0 {
            return self.head;
        }
        let rem = self.head.checked_rem(self.commit_interval).unwrap_or(0);
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

/// Build an [`ExecutionResults`] from a Go-emitted `exec_results` JSON object.
/// The gas-time is reconstructed at unit rate from the emitted Unix seconds (the
/// only component the settlement walk reads — settlement is decided on the gas
/// clock's whole-second instant, specs/11 §1.2).
fn exec_results_from_json(v: &serde_json::Value) -> Result<ExecutionResults, VectorError> {
    let gas_unix = u64_field(v, "gas_time_unix_seconds")?;
    let base_fee = u64_field(v, "base_fee")?;
    let receipt_root = parse_b256("receipt_root", str_field(v, "receipt_root")?)?;
    let post_state_root = parse_b256("post_state_root", str_field(v, "post_state_root")?)?;
    Ok(ExecutionResults {
        gas_time: Time::<u64>::new(gas_unix, 0, 1),
        base_fee: Price(base_fee),
        receipt_root,
        post_state_root,
    })
}

/// Replay a single Go-oracle recovery vector against the Rust SAE recovery
/// pipeline, returning the Rust-reconstructed frontier observation alongside the
/// Go source + recovered observations the corpus recorded.
///
/// # Errors
/// Returns [`VectorError`] on malformed corpus JSON, a block that fails to
/// parse, or a `recover()` failure.
pub async fn replay_recovery_vector(
    json: &str,
) -> Result<
    (
        FrontierObservation,
        FrontierObservation,
        FrontierObservation,
    ),
    VectorError,
> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| VectorError::Json(e.to_string()))?;

    let commit_interval = u64_field(&v, "commit_interval")?;
    let head = u64_field(&v, "chain_len")?;

    // ---- parse the synchronous genesis from its wire bytes ----
    let genesis_v = v
        .get("genesis")
        .ok_or(VectorError::MissingField("genesis"))?;
    let genesis_bytes = parse_hex(
        "genesis.wire_bytes_hex",
        str_field(genesis_v, "wire_bytes_hex")?,
    )?;
    let genesis_block =
        parse_block(&genesis_bytes, parse_now()).map_err(|source| VectorError::ParseBlock {
            height: u64_field(genesis_v, "height").unwrap_or(0),
            source,
        })?;
    let genesis = Arc::new(genesis_block);
    genesis
        .mark_synchronous((
            ava_vm::components::gas::Gas(0),
            ava_saevm_gastime::GasPriceConfig::default(),
        ))
        .map_err(VectorError::Genesis)?;

    // ---- parse every canonical height's body + execution results ----
    let mut canonical: BTreeMap<u64, (SealedBlock<RethBlock>, ExecutionResults)> = BTreeMap::new();
    let heights = v
        .get("heights")
        .and_then(serde_json::Value::as_array)
        .ok_or(VectorError::MissingField("heights"))?;
    for h in heights {
        let height = u64_field(h, "height")?;
        let bytes = parse_hex("heights[].wire_bytes_hex", str_field(h, "wire_bytes_hex")?)?;
        // parse_block re-seals (recomputes keccak256(RLP(header))) — block-hash
        // parity with the Go node is verified by construction here.
        let parsed = parse_block(&bytes, parse_now())
            .map_err(|source| VectorError::ParseBlock { height, source })?;
        let eth: SealedBlock<RethBlock> = parsed.eth_block().clone();
        let results = exec_results_from_json(
            h.get("exec_results")
                .ok_or(VectorError::MissingField("heights[].exec_results"))?,
        )?;
        canonical.insert(height, (eth, results));
    }

    // ---- drive the real Rust recovery ----
    let src = VectorRecoverySource {
        last_synchronous: Arc::clone(&genesis),
        head,
        commit_interval,
        canonical,
    };
    let recovered = recover(&src).await?;
    let f = &recovered.frontier;

    let rust = FrontierObservation {
        accepted_height: f.last_accepted().height(),
        executed_height: f.last_executed().map_or(0, |b| b.height()),
        settled_height: f.last_settled().height(),
        settled_hash: format!("0x{}", hex::encode(f.last_settled().hash().as_slice())),
        settled_state_root: format!(
            "0x{}",
            hex::encode(f.last_settled().post_execution_state_root().as_slice())
        ),
        executed_state_root: format!(
            "0x{}",
            hex::encode(
                f.last_executed()
                    .map_or(B256::ZERO, |b| b.post_execution_state_root())
                    .as_slice()
            )
        ),
    };

    let go_source = FrontierObservation::from_json(
        v.get("source").ok_or(VectorError::MissingField("source"))?,
    )?;
    let go_recovered = FrontierObservation::from_json(
        v.get("recovered")
            .ok_or(VectorError::MissingField("recovered"))?,
    )?;

    Ok((rust, go_source, go_recovered))
}

// ===========================================================================
// M7.30 streaming differential
// ===========================================================================

/// One `AwaitFinalization` barrier of the streaming differential: the Rust- and
/// Go-side A/E/S frontier observations after accepting + executing the block at
/// [`StreamingBarrier::height`]. The cross-implementation comparison is
/// `rust == go` (the test asserts equality at every barrier).
#[derive(Debug, Clone)]
pub struct StreamingBarrier {
    /// The accepted height this barrier observes.
    pub height: u64,
    /// The frontier the **Rust** pipeline reconstructed after this accept.
    pub rust: FrontierObservation,
    /// The frontier the **live Go node** observed after the same accept.
    pub go: FrontierObservation,
}

/// Snapshot the Rust [`Frontier`]'s current A/E/S as a [`FrontierObservation`]
/// (the per-barrier observation the streaming differential compares).
fn observe_frontier(frontier: &Frontier) -> FrontierObservation {
    let settled = frontier.last_settled();
    let executed = frontier.last_executed();
    let accepted = frontier.last_accepted();
    FrontierObservation {
        accepted_height: accepted.height(),
        executed_height: executed.as_ref().map_or(0, |b| b.height()),
        settled_height: settled.height(),
        settled_hash: format!("0x{}", hex::encode(settled.hash().as_slice())),
        settled_state_root: format!(
            "0x{}",
            hex::encode(settled.post_execution_state_root().as_slice())
        ),
        executed_state_root: format!(
            "0x{}",
            hex::encode(
                executed
                    .map_or(B256::ZERO, |b| b.post_execution_state_root())
                    .as_slice()
            )
        ),
    }
}

/// Build an [`ExecutionResults`] from a Go-emitted streaming `exec_results` JSON
/// object, reconstructing the gas-time at **full precision** (seconds +
/// fractional-second numerator/denominator). Unlike the recovery parser
/// ([`exec_results_from_json`], whole-second), the streaming settlement boundary
/// lands on a sub-second tie, so the fraction is consensus-critical (specs/11
/// §1.2): `Time::new(secs, frac_num, frac_denom)` reconstructs the exact
/// `proxytime.Time` the Go node executed by.
fn stream_exec_results_from_json(v: &serde_json::Value) -> Result<ExecutionResults, VectorError> {
    let gas_unix = u64_field(v, "gas_time_unix_seconds")?;
    let frac_num = u64_field(v, "gas_time_frac_num")?;
    let frac_denom = u64_field(v, "gas_time_frac_denom")?;
    let base_fee = u64_field(v, "base_fee")?;
    let receipt_root = parse_b256("receipt_root", str_field(v, "receipt_root")?)?;
    let post_state_root = parse_b256("post_state_root", str_field(v, "post_state_root")?)?;
    // A zero denominator would be a malformed corpus; fall back to unit rate
    // (frac then contributes nothing) rather than constructing a /0 clock.
    let hertz = if frac_denom == 0 { 1 } else { frac_denom };
    Ok(ExecutionResults {
        gas_time: Time::<u64>::new(gas_unix, frac_num, hertz),
        base_fee: Price(base_fee),
        receipt_root,
        post_state_root,
    })
}

/// Replay a Go-oracle **streaming** vector against the real Rust SAE pipeline:
/// drive the Go-emitted block stream block-by-block through a [`Frontier`] +
/// [`settle`](fn@settle) walk, returning one [`StreamingBarrier`] per accepted height
/// (the per-`AwaitFinalization`-barrier transcript) pairing the Rust- and
/// Go-side frontier observations.
///
/// At each barrier the driver: parses the Go block's wire bytes (re-sealing the
/// hash), marks it executed from the Go-emitted [`ExecutionResults`] (gas-time,
/// base fee, receipt/state roots), advances `LastAccepted`/`LastExecuted`, then
/// runs the Rust `settle()` walk on behalf of the freshly-accepted block — so
/// the **settlement choice** (which height becomes `LastSettled`) and the
/// **block hashes** are genuinely recomputed by Rust; the roots/base-fee
/// round-trip (see the test module docs).
///
/// # Errors
/// Returns [`VectorError`] on malformed corpus JSON, a block that fails to parse,
/// an inconsistent ancestry, or a `settle()` failure.
pub async fn replay_streaming_vector(json: &str) -> Result<Vec<StreamingBarrier>, VectorError> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| VectorError::Json(e.to_string()))?;

    // ---- parse the synchronous genesis from its wire bytes ----
    let genesis_v = v
        .get("genesis_block")
        .ok_or(VectorError::MissingField("genesis_block"))?;
    let genesis_bytes = parse_hex(
        "genesis_block.wire_bytes_hex",
        str_field(genesis_v, "wire_bytes_hex")?,
    )?;
    let genesis = Arc::new(parse_block(&genesis_bytes, parse_now()).map_err(|source| {
        VectorError::ParseBlock {
            height: u64_field(genesis_v, "height").unwrap_or(0),
            source,
        }
    })?);
    genesis
        .mark_synchronous((
            ava_vm::components::gas::Gas(0),
            ava_saevm_gastime::GasPriceConfig::default(),
        ))
        .map_err(VectorError::Genesis)?;

    let frontier = Frontier::new(Arc::clone(&genesis));

    // ---- drive the per-barrier stream ----
    let barriers_json = v
        .get("barriers")
        .and_then(serde_json::Value::as_array)
        .ok_or(VectorError::MissingField("barriers"))?;

    let mut parent = Arc::clone(&genesis);
    let mut out: Vec<StreamingBarrier> = Vec::with_capacity(barriers_json.len());

    for bj in barriers_json {
        let height = u64_field(bj, "height")?;
        let bytes = parse_hex(
            "barriers[].wire_bytes_hex",
            str_field(bj, "wire_bytes_hex")?,
        )?;
        // parse_block re-seals (recomputes keccak256(RLP(header))) — block-hash
        // parity with the Go node is verified by construction here.
        let parsed = parse_block(&bytes, parse_now())
            .map_err(|source| VectorError::ParseBlock { height, source })?;
        // Link ancestry: parent is the previous height; last_settled is left None
        // and re-derived by the settle() walk below (matching the recovery driver
        // + the Go `newCanonicalBlock(..., nil)` contract).
        let block = Arc::new(parsed);
        block
            .set_ancestors(Some(Arc::clone(&parent)), None)
            .map_err(|source| VectorError::Stream { height, source })?;

        // Mark executed from the Go-emitted committed results (gas-time decides
        // settlement; roots/base-fee round-trip). interim == committed gas-time.
        let results = stream_exec_results_from_json(
            bj.get("exec_results")
                .ok_or(VectorError::MissingField("barriers[].exec_results"))?,
        )?;
        let artefacts = ExecutionArtefacts {
            interim_execution_time: results.gas_time.clone(),
            results,
        };
        block
            .mark_executed(artefacts, None)
            .map_err(|source| VectorError::Stream { height, source })?;

        // Advance A then E (the frontier ignores stale advances).
        frontier.advance_accepted(&block);
        frontier.advance_executed(&block);

        // Run the real Rust settle() walk on behalf of the freshly-accepted block:
        // this recomputes the LastSettled choice from the gas-times + build-times.
        // ExecutionLagging cannot occur here — every ancestor is executed before
        // its child is accepted (synchronous per-barrier execution), matching the
        // Go oracle's `WaitUntilExecuted` between accepts.
        settle(&frontier, &block).map_err(|source| VectorError::Settle { height, source })?;

        let rust = observe_frontier(&frontier);
        let go = FrontierObservation::from_json(
            bj.get("frontier")
                .ok_or(VectorError::MissingField("barriers[].frontier"))?,
        )?;
        out.push(StreamingBarrier { height, rust, go });

        parent = block;
    }

    Ok(out)
}
