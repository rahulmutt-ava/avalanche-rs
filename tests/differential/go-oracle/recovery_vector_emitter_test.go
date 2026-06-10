// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package sae

// SAE crash+restart recovery vector emitter for the avalanche-rs M7.29
// `differential::sae_recovery` test.
//
// This is the LIVE Go oracle: it drives the real Go `vms/saevm` SAE node through
// a scripted block stream, crashes (snapshots the durable DB) at a chosen crash
// point, restarts via `recover()`, and writes a JSON `Observation` corpus that
// the Rust differential replays. The corpus carries, per height, the canonical
// block WIRE BYTES (so the Rust driver parses byte-identical blocks) plus the
// committed `ExecutionResults` (gas-time, base fee, receipt/state roots), and
// the SOURCE + RECOVERED A/E/S frontier heights/hashes. The Rust side feeds the
// exact Go bytes + results into its own `RecoverySource`, runs its `recover()`,
// and asserts the reconstructed settlement choice + A/E/S equal the Go oracle's.
//
// Gated behind `SAE_EMIT_RECOVERY_VECTORS=<output-dir>` so a normal `go test`
// run never executes it; re-freeze the corpus with:
//
//	SAE_EMIT_RECOVERY_VECTORS=/abs/tests/vectors/saevm/recovery_differential \
//	  go test ./vms/saevm/sae/ -run TestEmitRecoveryVectors -count=1
//
// The committed source-of-truth copy of this file lives in the avalanche-rs repo
// under tests/differential/go-oracle/; this copy is dropped into the avalanchego
// checkout (it needs the unexported `newSUT` / `rawVM` test harness) to run.

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

// execResultsJSON mirrors the consensus-critical, restart-surviving
// `ExecutionResults` (gas-time unix seconds + base fee + roots) for one height.
type execResultsJSON struct {
	GasTimeUnixSeconds uint64 `json:"gas_time_unix_seconds"`
	BaseFee            uint64 `json:"base_fee"`
	ReceiptRoot        string `json:"receipt_root"`
	PostStateRoot      string `json:"post_state_root"`
}

// heightJSON is one canonical (accepted) height: its wire bytes + the committed
// execution results. `parse_block(wire_bytes)` on the Rust side re-seals a
// byte-identical block (hash parity by construction).
type heightJSON struct {
	Height       uint64           `json:"height"`
	Hash         string           `json:"hash"`
	WireBytesHex string           `json:"wire_bytes_hex"`
	BuildTime    uint64           `json:"build_time"`
	Synchronous  bool             `json:"synchronous"`
	ExecResults  *execResultsJSON `json:"exec_results,omitempty"`
}

// frontierJSON is an A/E/S observation: the three frontier heights, the
// LastSettled block hash, and the settled/executed post-state roots.
type frontierJSON struct {
	AcceptedHeight    uint64 `json:"accepted_height"`
	ExecutedHeight    uint64 `json:"executed_height"`
	SettledHeight     uint64 `json:"settled_height"`
	SettledHash       string `json:"settled_hash"`
	SettledStateRoot  string `json:"settled_state_root"`
	ExecutedStateRoot string `json:"executed_state_root"`
}

// recoveryVectorJSON is the full emitted corpus for one (chain-length, crash
// point) case.
type recoveryVectorJSON struct {
	Comment        string       `json:"_comment"`
	GoCommit       string       `json:"go_commit"`
	TauSeconds     uint64       `json:"tau_seconds"`
	ChainLen       uint64       `json:"chain_len"`
	CommitInterval uint64       `json:"commit_interval"`
	CrashPoint     string       `json:"crash_point"`
	Seed           uint64       `json:"seed"`
	Genesis        heightJSON   `json:"genesis"`
	Heights        []heightJSON `json:"heights"`
	Source         frontierJSON `json:"source"`
	Recovered      frontierJSON `json:"recovered"`
}

func hexBytes(b []byte) string { return "0x" + common.Bytes2Hex(b) }

// observeFrontier reads the three live frontiers off a (raw) VM.
func observeFrontier(vm *VM) frontierJSON {
	accepted := vm.last.accepted.Load()
	settled := vm.last.settled.Load()
	executed := vm.exec.LastExecuted()
	return frontierJSON{
		AcceptedHeight:    accepted.Height(),
		ExecutedHeight:    executed.Height(),
		SettledHeight:     settled.Height(),
		SettledHash:       settled.Hash().Hex(),
		SettledStateRoot:  settled.PostExecutionStateRoot().Hex(),
		ExecutedStateRoot: executed.PostExecutionStateRoot().Hex(),
	}
}

// crashCase maps a named crash point to a commit interval. The crash point
// controls how much committed-execution-root state survives the restart; the
// Rust `RecoverySource::last_committed_height` rounds the head down to the
// nearest multiple of the interval, exactly as Go's
// `saedb.LastCommittedTrieDBHeight` does. Re-execution from the last committed
// root is pure, so all crash points must reconstruct the SAME final A/E/S.
type crashCase struct {
	name           string
	commitInterval uint64
}

func TestEmitRecoveryVectors(t *testing.T) {
	outDir := os.Getenv("SAE_EMIT_RECOVERY_VECTORS")
	if outDir == "" {
		t.Skip("set SAE_EMIT_RECOVERY_VECTORS=<output-dir> to emit the recovery differential corpus")
	}
	require.NoError(t, os.MkdirAll(outDir, 0o755), "MkdirAll(outDir)")

	const (
		commitInterval = 16
		chainLen       = 24 // > commitInterval so a non-archival restart re-executes from a boundary
		seed           = 0
	)

	cases := []crashCase{
		// mid-execute / between commit interval and head: the committed root sits
		// at the last interval boundary (16) below the head (24), so heights
		// (16, 24] are re-executed on restart.
		{name: "between_commit_and_head", commitInterval: commitInterval},
		// after-commit-before-pointer: archival cadence (commit every block) — the
		// committed root == head, nothing is re-executed.
		{name: "archival_after_commit", commitInterval: 1},
		// commit interval exactly: head is an exact multiple of the interval.
		{name: "commit_interval_exactly", commitInterval: chainLen},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			vec := emitOne(t, chainLen, c, seed)
			path := filepath.Join(outDir, fmt.Sprintf("recovery_%s.json", c.name))
			buf, err := json.MarshalIndent(vec, "", "  ")
			require.NoError(t, err, "json.MarshalIndent")
			require.NoError(t, os.WriteFile(path, append(buf, '\n'), 0o644), "WriteFile")
			t.Logf("wrote %s", path)
		})
	}
}

func emitOne(t *testing.T, chainLen uint64, c crashCase, seed uint64) recoveryVectorJSON {
	t.Helper()

	sutOpt, vmTime := withVMTime(t, time.Unix(saeparams.TauSeconds, 0))

	var srcDB database.Database
	srcHDB := saetest.NewHeightIndexDB()
	ctx, src := newSUT(t, 1, sutOpt, withExecResultsDB(srcHDB), withCommitInterval(c.commitInterval), options.Func[sutConfig](func(cfg *sutConfig) {
		srcDB = cfg.db
		cfg.logLevel = logging.Warn
		if c.commitInterval == 1 {
			cfg.vmConfig.DBConfig.Archival = true
		}
	}))

	rng := rand.New(rand.NewPCG(seed, 0)) //#nosec G404 -- deterministic test vector

	// Drive `chainLen` blocks, each with one revert-and-consume-all-gas contract
	// creation tx so execution genuinely advances the gas clock (mirrors the Go
	// recovery_test.go workload).
	for range chainLen {
		tx := src.wallet.SetNonceAndSign(t, 0, &types.LegacyTx{
			To:       nil,
			Data:     []byte{byte(vm.INVALID)},
			Gas:      params.TxGas + params.CreateGas + params.TxDataNonZeroGasFrontier + rng.Uint64N(2e6),
			GasPrice: big.NewInt(100),
		})
		vmTime.advance(850 * time.Millisecond)
		b := src.runConsensusLoop(t, tx)
		require.Len(t, b.Transactions(), 1, "transactions in block")
		require.NoErrorf(t, b.WaitUntilExecuted(ctx), "%T.WaitUntilExecuted()", b)
	}

	// Capture genesis wire bytes + per-height canonical bytes & execution results.
	genesis := src.genesis
	genJSON := heightJSON{
		Height:       genesis.Height(),
		Hash:         genesis.Hash().Hex(),
		WireBytesHex: hexBytes(genesis.Bytes()),
		BuildTime:    genesis.BuildTime(),
		Synchronous:  true,
	}

	var heights []heightJSON
	for h := genesis.Height() + 1; h <= chainLen; h++ {
		ethB, err := canonicalBlock(src.rawVM.db, h)
		require.NoErrorf(t, err, "canonicalBlock(%d)", h)
		b, err := blocks.New(ethB, nil, nil, src.logger)
		require.NoErrorf(t, err, "blocks.New(%d)", h)

		// Restore the committed execution artefacts (the durable, restart-surviving
		// half) so we can emit the consensus-critical roots + gas-time.
		xdb := saetypes.ExecutionResults{HeightIndex: srcHDB}
		require.NoErrorf(t, b.RestoreExecutionArtefacts(src.rawVM.db, xdb, src.rawVM.exec.ChainConfig()), "RestoreExecutionArtefacts(%d)", h)

		gasTime := b.ExecutedByGasTime()
		baseFee := b.ExecutedBaseFee()

		hj := heightJSON{
			Height:       h,
			Hash:         b.Hash().Hex(),
			WireBytesHex: hexBytes(b.Bytes()),
			BuildTime:    b.BuildTime(),
			Synchronous:  false,
			ExecResults: &execResultsJSON{
				GasTimeUnixSeconds: gasTime.Unix(),
				BaseFee:            baseFee.Uint64(),
				ReceiptRoot:        b.Header().ReceiptHash.Hex(),
				PostStateRoot:      b.PostExecutionStateRoot().Hex(),
			},
		}
		heights = append(heights, hj)
	}

	source := observeFrontier(src.rawVM)

	// ===== crash + restart: copy the durable DB, recover into a fresh SUT. =====
	newDB := copyDB(t, srcDB)
	_, sut := newSUT(t, 1, sutOpt, withExecResultsDB(srcHDB.Clone()), withCommitInterval(c.commitInterval), options.Func[sutConfig](func(cfg *sutConfig) {
		cfg.db = newDB
		cfg.logLevel = logging.Warn
		if c.commitInterval == 1 {
			cfg.vmConfig.DBConfig.Archival = true
		}
	}))
	recovered := observeFrontier(sut.rawVM)

	return recoveryVectorJSON{
		Comment:        "LIVE Go-oracle SAE crash+restart recovery vector for avalanche-rs differential::sae_recovery (M7.29). Per-height wire_bytes_hex are RLP-encoded geth blocks; the Rust driver parse_block's them (hash parity by construction) and feeds exec_results into its own RecoverySource::recover(), asserting the reconstructed A/E/S + settlement choice equal source==recovered here.",
		GoCommit:       goCommit(),
		TauSeconds:     saeparams.TauSeconds,
		ChainLen:       chainLen,
		CommitInterval: c.commitInterval,
		CrashPoint:     c.name,
		Seed:           seed,
		Genesis:        genJSON,
		Heights:        heights,
		Source:         source,
		Recovered:      recovered,
	}
}

// goCommit records the avalanchego HEAD commit (best-effort) for provenance.
func goCommit() string {
	if v := os.Getenv("AVALANCHEGO_COMMIT"); v != "" {
		return v
	}
	return "unknown"
}
