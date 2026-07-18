// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package evm

// Rust-built-block verdict judge for the avalanche-rs M9.15 Task 6 differential
// (`crates/ava-evm/tests/proposer_candidates.rs :: proposer_verdicts_hold`).
//
// This is the LIVE Go oracle for that differential — the reverse shape of the
// M7.29 recovery emitter (there the Go side EMITS a corpus for a Rust reader;
// here the Rust side emits candidate block RLPs and this file JUDGES them):
// it boots a real coreth test VM (`vmtest.SetupTestVM`, the same helper the
// package's own tests use) over the SAME genesis JSON the Rust candidates were
// built against (committed beside them as `genesis.json`), then for every
// `<name>.rlp.hex` candidate in the input directory calls `vm.ParseBlock` +
// `blk.Verify` and records `{name, accepted, error}`.
//
// Gated behind `RUST_BLOCK_VERDICT_DIR=<candidates-dir>` so a normal `go test`
// run never executes it; re-run the judge with:
//
//	RUST_BLOCK_VERDICT_DIR=/abs/crates/ava-evm/tests/vectors/proposer_verdict \
//	  go test -run TestRustBuiltBlockVerdicts ./graft/coreth/plugin/evm/ -v
//
// The committed source-of-truth copy of this file lives in the avalanche-rs
// repo under tests/differential/go-oracle/; this copy is dropped into the
// avalanchego checkout (`graft/coreth/plugin/evm/`, `package evm`) to run,
// because it needs the package's own unexported `newDefaultTestVM` helper.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	"github.com/ava-labs/libevm/common"
	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/graft/coreth/plugin/evm/vmtest"
	"github.com/ava-labs/avalanchego/upgrade/upgradetest"
)

// rustVerdictJSON is one candidate's judged outcome.
type rustVerdictJSON struct {
	Name     string `json:"name"`
	Accepted bool   `json:"accepted"`
	Error    string `json:"error,omitempty"`
}

// rustVerdictsFileJSON is the full emitted verdicts corpus.
type rustVerdictsFileJSON struct {
	Comment  string            `json:"_comment"`
	GoCommit string            `json:"go_commit"`
	Verdicts []rustVerdictJSON `json:"verdicts"`
}

// TestRustBuiltBlockVerdicts judges every `<name>.rlp.hex` candidate in
// $RUST_BLOCK_VERDICT_DIR against a real coreth VM booted on that directory's
// `genesis.json`, and writes `verdicts.json` back into the same directory.
func TestRustBuiltBlockVerdicts(t *testing.T) {
	dir := os.Getenv("RUST_BLOCK_VERDICT_DIR")
	if dir == "" {
		t.Skip("set RUST_BLOCK_VERDICT_DIR=<candidates-dir> to judge the Rust-built candidates")
	}

	genesisBytes, err := os.ReadFile(filepath.Join(dir, "genesis.json"))
	require.NoErrorf(t, err, "ReadFile(%s/genesis.json)", dir)

	entries, err := os.ReadDir(dir)
	require.NoErrorf(t, err, "ReadDir(%s)", dir)

	var names []string
	for _, e := range entries {
		if !e.IsDir() && strings.HasSuffix(e.Name(), ".rlp.hex") {
			names = append(names, strings.TrimSuffix(e.Name(), ".rlp.hex"))
		}
	}
	sort.Strings(names)
	require.NotEmpty(t, names, "at least one *.rlp.hex candidate in RUST_BLOCK_VERDICT_DIR")

	verdicts := make([]rustVerdictJSON, 0, len(names))
	for _, name := range names {
		t.Run(name, func(t *testing.T) {
			vm := newDefaultTestVM()
			fork := upgradetest.Granite
			vmtest.SetupTestVM(t, vm, vmtest.TestVMConfig{
				GenesisJSON: string(genesisBytes),
				Fork:        &fork,
			})
			defer func() {
				require.NoError(t, vm.Shutdown(t.Context()))
			}()

			rawHex, err := os.ReadFile(filepath.Join(dir, name+".rlp.hex"))
			require.NoErrorf(t, err, "ReadFile(%s.rlp.hex)", name)
			blockBytes := common.FromHex("0x" + strings.TrimPrefix(strings.TrimSpace(string(rawHex)), "0x"))
			require.NotEmptyf(t, blockBytes, "%s.rlp.hex decodes to non-empty bytes", name)

			v := rustVerdictJSON{Name: name}
			blk, err := vm.ParseBlock(t.Context(), blockBytes)
			switch {
			case err != nil:
				v.Accepted = false
				v.Error = err.Error()
			default:
				if verr := blk.Verify(t.Context()); verr != nil {
					v.Accepted = false
					v.Error = verr.Error()
				} else {
					v.Accepted = true
				}
			}
			verdicts = append(verdicts, v)
			t.Logf("%s: accepted=%v error=%q", name, v.Accepted, v.Error)
		})
	}

	out := rustVerdictsFileJSON{
		Comment:  "LIVE Go-oracle verdict leg for avalanche-rs M9.15 Task 6: coreth judges Rust-built C-Chain candidate blocks (the honest builder output + 10 adversarial header mutations) over the genesis.json committed beside this corpus.",
		GoCommit: rustVerdictGoCommit(),
		Verdicts: verdicts,
	}
	buf, err := json.MarshalIndent(out, "", "  ")
	require.NoError(t, err, "json.MarshalIndent")
	outPath := filepath.Join(dir, "verdicts.json")
	require.NoError(t, os.WriteFile(outPath, append(buf, '\n'), 0o644), "WriteFile(verdicts.json)")
	t.Logf("wrote %s", outPath)
}

// rustVerdictGoCommit records the avalanchego HEAD commit (best-effort) for
// provenance.
func rustVerdictGoCommit() string {
	if v := os.Getenv("AVALANCHEGO_COMMIT"); v != "" {
		return v
	}
	return "unknown"
}
