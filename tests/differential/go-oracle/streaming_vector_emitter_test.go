// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package sae

// SAE streaming-pipeline vector emitter for the avalanche-rs M7.30
// `differential::sae_streaming` test.
//
// This is the LIVE Go oracle for the STREAMING (pipelined-commit) comparison: it
// drives the real Go `vms/saevm` SAE node through a scripted block stream and,
// AFTER EVERY ACCEPTED BLOCK (each `AwaitFinalization` barrier), snapshots the
// A/E/S frontier observation plus that height's canonical block WIRE BYTES and
// committed `ExecutionResults`. The emitted corpus is a per-barrier transcript:
// an ordered array of `{height, wire_bytes, exec_results, frontier}` records, one
// per accepted height. The Rust differential drives its own frontier + settle()
// walk block-by-block over the same stream and asserts the reconstructed S/E/A +
// settlement choice + roots match the Go node's at EVERY barrier index — which
// validates the specs/00 §9 pipelined-commit optimization is observably neutral.
//
// Gated behind `SAE_EMIT_STREAMING_VECTORS=<output-dir>` so a normal `go test`
// run never executes it; re-freeze the corpus with:
//
//	SAE_EMIT_STREAMING_VECTORS=/abs/tests/vectors/saevm/streaming_differential \
//	  go test ./vms/saevm/sae/ -run TestEmitStreamingVectors -count=1
//
// The committed source-of-truth copy of this file lives in the avalanche-rs repo
// under tests/differential/go-oracle/; this copy is dropped into the avalanchego
// checkout (it needs the unexported `newSUT` / `rawVM` test harness) to run. It
// shares the JSON shapes (`execResultsJSON`, `heightJSON`, `frontierJSON`) and
// the `observeFrontier` / `hexBytes` helpers with the recovery emitter — only
// drop ONE emitter into the checkout at a time (they redeclare those helpers).

import (
	"encoding/json"
	"fmt"
	"math/big"
	"math/rand/v2"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/ava-labs/libevm/common"
	"github.com/ava-labs/libevm/core/types"
	"github.com/ava-labs/libevm/core/vm"
	"github.com/ava-labs/libevm/libevm/options"
	"github.com/ava-labs/libevm/params"
	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/database"
	"github.com/ava-labs/avalanchego/utils/logging"
	"github.com/ava-labs/avalanchego/vms/saevm/blocks"
	"github.com/ava-labs/avalanchego/vms/saevm/saetest"

	saeparams "github.com/ava-labs/avalanchego/vms/saevm/params"
	saetypes "github.com/ava-labs/avalanchego/vms/saevm/types"
)

// streamExecResultsJSON mirrors the consensus-critical, restart-surviving
// `ExecutionResults` for one height. The settlement-deciding gas-time is emitted
// at FULL precision (seconds + fractional-second numerator + denominator/hertz),
// not just whole Unix seconds: the per-barrier settlement boundary lands exactly
// on a sub-second tie, so the fraction is consensus-critical here (unlike the
// recovery emitter, which only needed whole seconds — specs/11 §1.2).
type streamExecResultsJSON struct {
	GasTimeUnixSeconds uint64 `json:"gas_time_unix_seconds"`
	GasTimeFracNum     uint64 `json:"gas_time_frac_num"`
	GasTimeFracDenom   uint64 `json:"gas_time_frac_denom"`
	BaseFee            uint64 `json:"base_fee"`
	ReceiptRoot        string `json:"receipt_root"`
	PostStateRoot      string `json:"post_state_root"`
}

// streamFrontierJSON is an A/E/S observation: the three frontier heights, the
// LastSettled block hash, and the settled/executed post-state roots.
type streamFrontierJSON struct {
	AcceptedHeight    uint64 `json:"accepted_height"`
	ExecutedHeight    uint64 `json:"executed_height"`
	SettledHeight     uint64 `json:"settled_height"`
	SettledHash       string `json:"settled_hash"`
	SettledStateRoot  string `json:"settled_state_root"`
	ExecutedStateRoot string `json:"executed_state_root"`
}

// barrierJSON is one `AwaitFinalization` barrier: the height just accepted, its
// canonical block WIRE BYTES + build time, the committed `ExecutionResults`, and
// the full A/E/S frontier observed AFTER that accept (and its execution). The
// Rust side parse_block's the wire bytes (hash parity by construction), marks the
// block executed with `exec_results.gas_time`, runs its own settle() walk, and
// asserts the reconstructed frontier == this `frontier`.
type barrierJSON struct {
	Height       uint64                 `json:"height"`
	Hash         string                 `json:"hash"`
	WireBytesHex string                 `json:"wire_bytes_hex"`
	BuildTime    uint64                 `json:"build_time"`
	ExecResults  *streamExecResultsJSON `json:"exec_results"`
	Frontier     streamFrontierJSON     `json:"frontier"`
}

// streamingVectorJSON is the full emitted corpus for one scripted stream.
type streamingVectorJSON struct {
	Comment        string             `json:"_comment"`
	GoCommit       string             `json:"go_commit"`
	TauSeconds     uint64             `json:"tau_seconds"`
	ChainLen       uint64             `json:"chain_len"`
	CommitInterval uint64             `json:"commit_interval"`
	Seed           uint64             `json:"seed"`
	StreamName     string             `json:"stream_name"`
	Genesis        streamFrontierJSON `json:"genesis"`
	GenesisBlock   barrierGenesisJSON `json:"genesis_block"`
	Barriers       []barrierJSON      `json:"barriers"`
}

// barrierGenesisJSON carries the synchronous (last pre-SAE) block the Rust side
// roots its frontier at: its wire bytes + the genesis frontier observation.
type barrierGenesisJSON struct {
	Height       uint64 `json:"height"`
	Hash         string `json:"hash"`
	WireBytesHex string `json:"wire_bytes_hex"`
	BuildTime    uint64 `json:"build_time"`
}

func streamHexBytes(b []byte) string { return "0x" + common.Bytes2Hex(b) }

// observeStreamFrontier reads the three live frontiers off a (raw) VM.
func observeStreamFrontier(vm *VM) streamFrontierJSON {
	accepted := vm.last.accepted.Load()
	settled := vm.last.settled.Load()
	executed := vm.exec.LastExecuted()
	return streamFrontierJSON{
		AcceptedHeight:    accepted.Height(),
		ExecutedHeight:    executed.Height(),
		SettledHeight:     settled.Height(),
		SettledHash:       settled.Hash().Hex(),
		SettledStateRoot:  settled.PostExecutionStateRoot().Hex(),
		ExecutedStateRoot: executed.PostExecutionStateRoot().Hex(),
	}
}

// streamCase is a named, scripted streaming workload: a per-block wall-clock
// advance and commit interval. The advance controls how fast block (build) times
// progress, which — relative to Tau — determines how the S-frontier trails the
// A-frontier across the barriers.
type streamCase struct {
	name           string
	commitInterval uint64
	advance        time.Duration
}

func TestEmitStreamingVectors(t *testing.T) {
	outDir := os.Getenv("SAE_EMIT_STREAMING_VECTORS")
	if outDir == "" {
		t.Skip("set SAE_EMIT_STREAMING_VECTORS=<output-dir> to emit the streaming differential corpus")
	}
	require.NoError(t, os.MkdirAll(outDir, 0o755), "MkdirAll(outDir)")

	const (
		chainLen = 24
		seed     = 0
	)

	cases := []streamCase{
		// All cases advance the wall clock by WHOLE seconds per block so the block
		// (build) time stays whole-second (the test stub's sub-second header
		// component is zero) — matching both the Go PRODUCTION cchain hook (which
		// uses whole-second BlockTime, hook.go::BlockTime TODO) and the Rust
		// `Block::timestamp()` whole-second model. The settlement-deciding gas-time
		// still carries its real sub-second fraction (it is the gas clock, not wall
		// time), which IS emitted + compared. specs/11 §1.2.
		//
		// steady_settling: 1s/block; with Tau=5s the S-frontier trails A by ~Tau
		// blocks, so settlement advances on most barriers (non-trivial S
		// trajectory).
		{name: "steady_settling", commitInterval: 16, advance: time.Second},
		// archival: commit every block (commit interval 1) — the executed root is
		// always durable; the per-barrier E-frontier tracks A tightly. Same cadence
		// as steady_settling, so it MUST reach an identical final A/E/S (the
		// pipelined-commit optimization is observably neutral, specs/00 §9).
		{name: "archival", commitInterval: 1, advance: time.Second},
		// fast_blocks: 2s/block — fewer blocks fit inside the Tau window, so the
		// S-frontier advances in larger steps per barrier.
		{name: "fast_blocks", commitInterval: 16, advance: 2 * time.Second},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			vec := emitStream(t, chainLen, c, seed)
			path := filepath.Join(outDir, fmt.Sprintf("streaming_%s.json", c.name))
			buf, err := json.MarshalIndent(vec, "", "  ")
			require.NoError(t, err, "json.MarshalIndent")
			require.NoError(t, os.WriteFile(path, append(buf, '\n'), 0o644), "WriteFile")
			t.Logf("wrote %s", path)
		})
	}
}

func emitStream(t *testing.T, chainLen uint64, c streamCase, seed uint64) streamingVectorJSON {
	t.Helper()

	sutOpt, vmTime := withVMTime(t, time.Unix(saeparams.TauSeconds, 0))

	var srcDB database.Database
	_ = srcDB
	srcHDB := saetest.NewHeightIndexDB()
	ctx, src := newSUT(t, 1, sutOpt, withExecResultsDB(srcHDB), withCommitInterval(c.commitInterval), options.Func[sutConfig](func(cfg *sutConfig) {
		srcDB = cfg.db
		cfg.logLevel = logging.Warn
		if c.commitInterval == 1 {
			cfg.vmConfig.DBConfig.Archival = true
		}
	}))

	rng := rand.New(rand.NewPCG(seed, 0)) //#nosec G404 -- deterministic test vector

	genesis := src.genesis
	out := streamingVectorJSON{
		Comment:        "LIVE Go-oracle SAE STREAMING per-barrier transcript for avalanche-rs differential::sae_streaming (M7.30). Each barriers[] entry is one accepted height: wire_bytes_hex (RLP geth block, the Rust side parse_block's it — hash parity by construction), exec_results (committed gas-time/base-fee/roots), and the A/E/S frontier observed AFTER that accept. The Rust differential drives its own frontier + settle() walk block-by-block and asserts the reconstructed S/E/A + settlement choice + roots equal frontier[] at EVERY index, validating the specs/00 §9 pipelined-commit optimization is observably neutral.",
		GoCommit:       streamGoCommit(),
		TauSeconds:     saeparams.TauSeconds,
		ChainLen:       chainLen,
		CommitInterval: c.commitInterval,
		Seed:           seed,
		StreamName:     c.name,
		GenesisBlock: barrierGenesisJSON{
			Height:       genesis.Height(),
			Hash:         genesis.Hash().Hex(),
			WireBytesHex: streamHexBytes(genesis.Bytes()),
			BuildTime:    genesis.BuildTime(),
		},
	}
	out.Genesis = observeStreamFrontier(src.rawVM)

	// Drive `chainLen` blocks one at a time, snapshotting the per-barrier
	// transcript AFTER each accept + execution. Each block carries one
	// revert-and-consume-all-gas contract-creation tx so execution genuinely
	// advances the gas clock (mirrors the Go recovery_test.go workload).
	for h := genesis.Height() + 1; h <= chainLen; h++ {
		tx := src.wallet.SetNonceAndSign(t, 0, &types.LegacyTx{
			To:       nil,
			Data:     []byte{byte(vm.INVALID)},
			Gas:      params.TxGas + params.CreateGas + params.TxDataNonZeroGasFrontier + rng.Uint64N(2e6),
			GasPrice: big.NewInt(100),
		})
		vmTime.advance(c.advance)
		b := src.runConsensusLoop(t, tx)
		require.Len(t, b.Transactions(), 1, "transactions in block")
		require.NoErrorf(t, b.WaitUntilExecuted(ctx), "%T.WaitUntilExecuted()", b)

		// Capture this height's canonical bytes + committed execution results.
		ethB, err := canonicalBlock(src.rawVM.db, h)
		require.NoErrorf(t, err, "canonicalBlock(%d)", h)
		cb, err := blocks.New(ethB, nil, nil, src.logger)
		require.NoErrorf(t, err, "blocks.New(%d)", h)
		xdb := saetypes.ExecutionResults{HeightIndex: srcHDB}
		require.NoErrorf(t, cb.RestoreExecutionArtefacts(src.rawVM.db, xdb, src.rawVM.exec.ChainConfig()), "RestoreExecutionArtefacts(%d)", h)

		gasTime := cb.ExecutedByGasTime()
		baseFee := cb.ExecutedBaseFee()
		frac := gasTime.Fraction()

		out.Barriers = append(out.Barriers, barrierJSON{
			Height:       h,
			Hash:         cb.Hash().Hex(),
			WireBytesHex: streamHexBytes(cb.Bytes()),
			BuildTime:    cb.BuildTime(),
			ExecResults: &streamExecResultsJSON{
				GasTimeUnixSeconds: gasTime.Unix(),
				GasTimeFracNum:     uint64(frac.Numerator),
				GasTimeFracDenom:   uint64(frac.Denominator),
				BaseFee:            baseFee.Uint64(),
				ReceiptRoot:        cb.Header().ReceiptHash.Hex(),
				PostStateRoot:      cb.PostExecutionStateRoot().Hex(),
			},
			// The frontier AFTER accepting + executing this block — the
			// per-AwaitFinalization-barrier observation.
			Frontier: observeStreamFrontier(src.rawVM),
		})
	}

	return out
}

// streamGoCommit records the avalanchego HEAD commit (best-effort) for
// provenance.
func streamGoCommit() string {
	if v := os.Getenv("AVALANCHEGO_COMMIT"); v != "" {
		return v
	}
	return "unknown"
}
