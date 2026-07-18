// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package customheader

// ACP-176 base-fee time-advance vector emitter for the avalanche-rs Task 7
// false-reject-guard differential
// (`crates/ava-evm/tests/feerules.rs :: acp176_base_fee_advance_matches_go_vectors`).
//
// This is the LIVE Go oracle proving the Task 1 fix to `feerules::base_fee`'s
// ACP-176 arm: an honest Go-computed base fee for a parent fee state with
// NONZERO excess must equal Rust's recompute after advancing the parent state
// by elapsed time (`fee_state_before_block`) — NOT a raw read of the parent's
// claimed price, which is what would falsely reject an honest Go block under
// load. It sweeps a Cartesian grid of (excess, elapsed-ms) over the real
// coreth `customheader.BaseFee`, with a Fortuna+Granite-active-at-genesis
// ChainConfig (`extras.TestGraniteChainConfig`) so the child fee state always
// advances by MILLISECONDS (exercising the Granite sub-second-granularity
// path, including the 500ms rows).
//
// Gated behind `BASE_FEE_ADVANCE_OUT=<abs path to base_fee_advance.json>` so a
// normal `go test` run never executes it; re-freeze the corpus with:
//
//	BASE_FEE_ADVANCE_OUT=/abs/crates/ava-evm/tests/vectors/cchain/fees/acp176/base_fee_advance.json \
//	  go test ./graft/coreth/plugin/evm/customheader/ -run TestEmitBaseFeeAdvanceVectors -count=1 -v
//
// The committed source-of-truth copy of this file lives in the avalanche-rs
// repo under tests/differential/go-oracle/; this copy is dropped into the
// avalanchego checkout (`graft/coreth/plugin/evm/customheader/`, `package
// customheader`) to run. Unlike the SAE emitters, it needs no unexported test
// harness — only the package's own EXPORTED `BaseFee`.

import (
	"encoding/hex"
	"encoding/json"
	"math/big"
	"os"
	"testing"

	"github.com/ava-labs/libevm/core/types"
	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/graft/coreth/params/extras"
	"github.com/ava-labs/avalanchego/vms/components/gas"
	"github.com/ava-labs/avalanchego/vms/evm/acp176"
)

// baseFeeAdvanceRowJSON is one (parent fee state, elapsed) sample: the coreth
// input parent header (as ACP-176 `Extra` bytes) + child timestamp, and the
// base fee coreth's `customheader.BaseFee` computed for it.
type baseFeeAdvanceRowJSON struct {
	ParentNumber    uint64 `json:"parent_number"`
	ParentTime      uint64 `json:"parent_time"`
	ParentExtraHex  string `json:"parent_extra_hex"`
	ChildTimeMS     uint64 `json:"child_time_ms"`
	ExpectedBaseFee uint64 `json:"expected_base_fee"`
}

// baseFeeAdvanceFileJSON is the full emitted corpus.
type baseFeeAdvanceFileJSON struct {
	Comment  string                  `json:"_comment"`
	GoCommit string                  `json:"go_commit"`
	Rows     []baseFeeAdvanceRowJSON `json:"rows"`
}

// TestEmitBaseFeeAdvanceVectors sweeps an excess x elapsed-ms grid through the
// real coreth `customheader.BaseFee` and writes the recorded corpus to
// $BASE_FEE_ADVANCE_OUT.
func TestEmitBaseFeeAdvanceVectors(t *testing.T) {
	out := os.Getenv("BASE_FEE_ADVANCE_OUT")
	if out == "" {
		t.Skip("set BASE_FEE_ADVANCE_OUT=<abs path> to emit the base-fee advance vector corpus")
	}

	config := extras.TestGraniteChainConfig // Fortuna + Granite active at genesis (timestamp 0).

	const (
		capacity     = gas.Gas(2_000_000)
		targetExcess = gas.Gas(1_500_000)
		// parentTime matches the Rust reader's `local_all_active_spec()` genesis
		// (the local network's real `InitiallyActiveTime`, 2020-12-05 05:00:00
		// UTC — see `feerules.rs`'s identical convention), so that spec's
		// Fortuna+Granite activation gate is satisfied by these rows. coreth's
		// `TestGraniteChainConfig` itself activates Fortuna+Granite at genesis
		// (timestamp 0), so any parentTime works Go-side; this value is chosen
		// only to keep the Rust-side reader on the intended ACP-176 branch.
		parentTime = uint64(1_607_144_400)
	)

	excesses := []gas.Gas{0, 1_000_000, 50_000_000, 200_000_000, 2_000_000_000}
	deltasMS := []uint64{0, 500, 1_000, 10_000, 60_000, 600_000}

	var rows []baseFeeAdvanceRowJSON
	for _, excess := range excesses {
		for _, deltaMS := range deltasMS {
			extra := (&acp176.State{
				Gas: gas.State{
					Capacity: capacity,
					Excess:   excess,
				},
				TargetExcess: targetExcess,
			}).Bytes()

			parent := &types.Header{
				Number: big.NewInt(1),
				Time:   parentTime,
				Extra:  extra,
			}
			childMS := parentTime*1000 + deltaMS

			bf, err := BaseFee(config, parent, childMS)
			require.NoErrorf(t, err, "BaseFee(excess=%d, deltaMS=%d)", excess, deltaMS)
			require.NotNilf(t, bf, "BaseFee(excess=%d, deltaMS=%d)", excess, deltaMS)
			require.Truef(t, bf.IsUint64(), "BaseFee(excess=%d, deltaMS=%d) fits in uint64", excess, deltaMS)

			rows = append(rows, baseFeeAdvanceRowJSON{
				ParentNumber:    parent.Number.Uint64(),
				ParentTime:      parent.Time,
				ParentExtraHex:  hex.EncodeToString(extra),
				ChildTimeMS:     childMS,
				ExpectedBaseFee: bf.Uint64(),
			})
		}
	}

	fileOut := baseFeeAdvanceFileJSON{
		Comment: "LIVE Go-oracle ACP-176 base-fee time-advance corpus for avalanche-rs Task 7 " +
			"(verifyHeaderGasFields plan): the false-reject-guard regression proof for the Task-1 " +
			"`feerules::base_fee` fix. Sweeps excess x elapsed-ms over the real coreth " +
			"customheader.BaseFee with a Fortuna+Granite-active-at-genesis ChainConfig. Rows with " +
			"excess > 0 and delta_ms > 0 must show a LOWER expected_base_fee than their delta_ms == 0 " +
			"sibling (AdvanceMilliseconds drains excess over elapsed time) — the case where a raw read " +
			"of the parent's claimed price (instead of advancing it) would falsely reject an honest Go " +
			"block under load.",
		GoCommit: baseFeeAdvanceGoCommit(),
		Rows:     rows,
	}
	buf, err := json.MarshalIndent(fileOut, "", "  ")
	require.NoError(t, err, "json.MarshalIndent")
	require.NoError(t, os.WriteFile(out, append(buf, '\n'), 0o644), "WriteFile")
	t.Logf("wrote %s (%d rows)", out, len(rows))
}

// baseFeeAdvanceGoCommit records the avalanchego HEAD commit (best-effort) for
// provenance.
func baseFeeAdvanceGoCommit() string {
	if v := os.Getenv("AVALANCHEGO_COMMIT"); v != "" {
		return v
	}
	return "unknown"
}
