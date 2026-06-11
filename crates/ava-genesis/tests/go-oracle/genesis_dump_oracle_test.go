// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package genesis

// Genesis golden-vector emitter for the avalanche-rs `ava-genesis` M8.8 tests
// (`tests/golden_genesis_block_id.rs::{genesis_block_id,
// genesis_p_chain_bytes_byte_identical}`).
//
// This is the Go oracle: for Mainnet, Fuji, Local (pre-start-time-advance) and
// the custom `genesis_test.json` (networkID 9999) it calls the real
// `genesis.FromConfig` and dumps the P-Chain genesis byte stream
// (`p_chain_bytes_<name>.bin`) plus every derived golden id
// (`block_ids.json`: P genesis block id, X/C blockchain ids, AVAX asset id,
// sha256 hex of the bytes).
//
// Gated behind `GENESIS_EMIT_VECTORS=<output-dir>` so a normal `go test` run
// never executes it. The committed source-of-truth copy of this file lives in
// the avalanche-rs repo under `crates/ava-genesis/tests/go-oracle/`; it must be
// dropped into the avalanchego `genesis/` package to run (it reads the
// unexported `unmodifiedLocalConfig` and the embedded `genesis_test.json`).
// Re-freeze with `cargo xtask gen-genesis` (or by hand):
//
//	cp crates/ava-genesis/tests/go-oracle/genesis_dump_oracle_test.go \
//	   "$AVALANCHEGO_DIR/genesis/"
//	cd "$AVALANCHEGO_DIR"
//	AVALANCHEGO_COMMIT=$(git rev-parse HEAD) \
//	GENESIS_EMIT_VECTORS=/abs/crates/ava-genesis/tests/vectors/genesis \
//	  go test ./genesis/ -run TestEmitGenesisVectors -count=1

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/ids"
	"github.com/ava-labs/avalanchego/utils/constants"
	"github.com/ava-labs/avalanchego/utils/hashing"
)

type emittedGenesisIDs struct {
	NetworkID            uint32 `json:"networkID"`
	PChainGenesisBlockID string `json:"pChainGenesisBlockID"`
	XBlockchainID        string `json:"xBlockchainID"`
	CBlockchainID        string `json:"cBlockchainID"`
	AVAXAssetID          string `json:"avaxAssetID"`
	PChainBytesSha256Hex string `json:"pChainBytesSha256Hex"`
	PChainBytesLen       int    `json:"pChainBytesLen"`
}

type emittedGenesisVectors struct {
	AvalanchegoCommit string                       `json:"avalanchegoCommit"`
	Emitter           string                       `json:"emitter"`
	Networks          map[string]emittedGenesisIDs `json:"networks"`
}

func TestEmitGenesisVectors(t *testing.T) {
	outDir := os.Getenv("GENESIS_EMIT_VECTORS")
	if outDir == "" {
		t.Skip("GENESIS_EMIT_VECTORS not set; skipping the avalanche-rs vector emitter")
	}
	require := require.New(t)
	require.NoError(os.MkdirAll(outDir, 0o755))

	customConfig, err := parseGenesisJSONBytesToConfig(customGenesisConfigJSON)
	require.NoError(err)

	cases := []struct {
		name   string
		config *Config
	}{
		{name: "mainnet", config: &MainnetConfig},
		{name: "fuji", config: &FujiConfig},
		// The pre-advance local config — the golden identity (the live
		// LocalConfig start time is advanced in 9-month chunks).
		{name: "local_unmodified", config: &unmodifiedLocalConfig},
		{name: "custom_9999", config: customConfig},
	}

	out := emittedGenesisVectors{
		AvalanchegoCommit: os.Getenv("AVALANCHEGO_COMMIT"),
		Emitter:           "avalanche-rs crates/ava-genesis/tests/go-oracle/genesis_dump_oracle_test.go (M8.8)",
		Networks:          make(map[string]emittedGenesisIDs, len(cases)),
	}
	for _, c := range cases {
		genesisBytes, avaxAssetID, err := FromConfig(c.config)
		require.NoError(err)
		require.NoError(os.WriteFile(
			filepath.Join(outDir, "p_chain_bytes_"+c.name+".bin"),
			genesisBytes,
			0o644,
		))

		xTx, err := VMGenesis(genesisBytes, constants.AVMID)
		require.NoError(err)
		cTx, err := VMGenesis(genesisBytes, constants.EVMID)
		require.NoError(err)

		var genesisID ids.ID = hashing.ComputeHash256Array(genesisBytes)
		out.Networks[c.name] = emittedGenesisIDs{
			NetworkID:            c.config.NetworkID,
			PChainGenesisBlockID: genesisID.String(),
			XBlockchainID:        xTx.ID().String(),
			CBlockchainID:        cTx.ID().String(),
			AVAXAssetID:          avaxAssetID.String(),
			PChainBytesSha256Hex: hex.EncodeToString(hashing.ComputeHash256(genesisBytes)),
			PChainBytesLen:       len(genesisBytes),
		}
	}

	encoded, err := json.MarshalIndent(out, "", "  ")
	require.NoError(err)
	require.NoError(os.WriteFile(filepath.Join(outDir, "block_ids.json"), append(encoded, '\n'), 0o644))
}
