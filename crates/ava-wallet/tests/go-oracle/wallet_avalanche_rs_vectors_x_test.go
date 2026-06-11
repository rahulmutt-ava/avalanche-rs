// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

// SCRATCH emitter for avalanche-rs golden wallet vectors (M8.26).
// Run from an avalanchego checkout (copy into wallet/chain/x/):
//
//	AVAX_RS_OUT=/tmp/x.json go test -run TestAvalancheRsXVectors ./wallet/chain/x/
//
// Delete from the Go tree after the vectors are committed to avalanche-rs
// (crates/ava-wallet/tests/vectors/wallet/x.json).

package x

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"os"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/ids"
	"github.com/ava-labs/avalanchego/utils/constants"
	"github.com/ava-labs/avalanchego/utils/crypto/secp256k1"
	"github.com/ava-labs/avalanchego/utils/set"
	"github.com/ava-labs/avalanchego/utils/units"
	"github.com/ava-labs/avalanchego/vms/avm/txs"
	"github.com/ava-labs/avalanchego/vms/components/avax"
	"github.com/ava-labs/avalanchego/vms/components/verify"
	"github.com/ava-labs/avalanchego/vms/secp256k1fx"
	"github.com/ava-labs/avalanchego/wallet/chain/x/builder"
	xsigner "github.com/ava-labs/avalanchego/wallet/chain/x/signer"
	"github.com/ava-labs/avalanchego/wallet/subnet/primary/common"
	"github.com/ava-labs/avalanchego/wallet/subnet/primary/common/utxotest"
)

type rsxVector struct {
	Name        string            `json:"name"`
	Inputs      map[string]string `json:"inputs,omitempty"`
	UnsignedHex string            `json:"unsigned_hex"`
	SignedHex   string            `json:"signed_hex"`
}

type rsxVectorFile struct {
	Provenance map[string]string `json:"provenance"`
	Vectors    []rsxVector       `json:"vectors"`
}

const rsxMinIssuanceTime uint64 = 1_700_000_000

var (
	rsxAvaxAssetID  = ids.Empty.Prefix(1789)
	rsxOtherAssetID = ids.Empty.Prefix(2024)
	rsxChainID      = ids.Empty.Prefix(2021)
	rsxOtherChainID = ids.Empty.Prefix(6161)
	rsxSmallChainID = ids.Empty.Prefix(6262)
)

func rsxContext() *builder.Context {
	return &builder.Context{
		NetworkID:        constants.UnitTestID,
		BlockchainID:     rsxChainID,
		AVAXAssetID:      rsxAvaxAssetID,
		BaseTxFee:        units.MicroAvax,
		CreateAssetTxFee: 99 * units.MilliAvax,
	}
}

func rsxSecpUTXO(prefix uint64, assetID ids.ID, amt uint64, addr ids.ShortID) *avax.UTXO {
	return &avax.UTXO{
		UTXOID: avax.UTXOID{TxID: ids.Empty.Prefix(prefix), OutputIndex: uint32(prefix)},
		Asset:  avax.Asset{ID: assetID},
		Out: &secp256k1fx.TransferOutput{
			Amt: amt,
			OutputOwners: secp256k1fx.OutputOwners{
				Threshold: 1,
				Addrs:     []ids.ShortID{addr},
			},
		},
	}
}

func TestAvalancheRsXVectors(t *testing.T) {
	out := os.Getenv("AVAX_RS_OUT")
	if out == "" {
		t.Skip("AVAX_RS_OUT not set")
	}
	require := require.New(t)
	ctx := context.Background()

	keys := secp256k1.TestKeys()
	recipientKey, utxoKey := keys[0], keys[1]
	recipientAddr := recipientKey.Address()
	utxoAddr := utxoKey.Address()

	utxoOwner := secp256k1fx.OutputOwners{Threshold: 1, Addrs: []ids.ShortID{utxoAddr}}
	recipientOwner := &secp256k1fx.OutputOwners{Threshold: 1, Addrs: []ids.ShortID{recipientAddr}}

	// The X-chain UTXO set: AVAX (fee + transfers) and one other fungible asset.
	chainUTXOs := []*avax.UTXO{
		rsxSecpUTXO(2024, rsxAvaxAssetID, 2*units.MilliAvax, utxoAddr),
		rsxSecpUTXO(2025, rsxOtherAssetID, 99*units.MegaAvax, utxoAddr),
		rsxSecpUTXO(2026, rsxAvaxAssetID, 9*units.Avax, utxoAddr),
	}
	// Importable UTXOs: AVAX above the base fee + another asset.
	importUTXOs := []*avax.UTXO{
		rsxSecpUTXO(3024, rsxAvaxAssetID, 2*units.MilliAvax, utxoAddr),
		rsxSecpUTXO(3025, rsxOtherAssetID, 5*units.Avax, utxoAddr),
	}
	// Importable AVAX strictly below the base fee (local-fee-top-up branch).
	smallImportUTXOs := []*avax.UTXO{
		rsxSecpUTXO(4024, rsxAvaxAssetID, 600*units.NanoAvax, utxoAddr),
	}

	avaxOutput := &avax.TransferableOutput{
		Asset: avax.Asset{ID: rsxAvaxAssetID},
		Out: &secp256k1fx.TransferOutput{
			Amt:          7 * units.Avax,
			OutputOwners: utxoOwner,
		},
	}

	kc := secp256k1fx.NewKeychain(keys...)
	addrs := set.Of(utxoAddr)
	testContext := rsxContext()

	opts := []common.Option{
		common.WithMinIssuanceTime(rsxMinIssuanceTime),
		common.WithChangeOwner(&utxoOwner),
	}

	newEnv := func() (builder.Builder, xsigner.Signer) {
		sets := map[ids.ID][]*avax.UTXO{
			rsxChainID:      chainUTXOs,
			rsxOtherChainID: importUTXOs,
			rsxSmallChainID: smallImportUTXOs,
		}
		deterministicUTXOs := utxotest.NewDeterministicChainUTXOs(t, sets)
		backend := NewBackend(testContext, deterministicUTXOs)
		return builder.New(addrs, testContext, backend), xsigner.New(kc, backend)
	}

	var vectors []rsxVector
	emit := func(name string, s xsigner.Signer, utx txs.UnsignedTx, buildErr error, inputs map[string]string) {
		require.NoError(buildErr, name)
		tx, err := xsigner.SignUnsigned(ctx, s, utx)
		require.NoError(err, name)
		unsignedBytes, err := builder.Parser.Codec().Marshal(txs.CodecVersion, &utx)
		require.NoError(err, name)
		vectors = append(vectors, rsxVector{
			Name:        name,
			Inputs:      inputs,
			UnsignedHex: hex.EncodeToString(unsignedBytes),
			SignedHex:   hex.EncodeToString(tx.Bytes()),
		})
	}

	// --- base ---
	{
		b, s := newEnv()
		utx, err := b.NewBaseTx([]*avax.TransferableOutput{avaxOutput}, opts...)
		emit("x_base", s, utx, err, nil)
	}
	// --- base with memo ---
	{
		b, s := newEnv()
		utx, err := b.NewBaseTx(
			[]*avax.TransferableOutput{avaxOutput},
			append(opts, common.WithMemo([]byte("memo")))...,
		)
		emit("x_base_memo", s, utx, err, nil)
	}
	// --- create_asset (secp mint + transfer initial state, fx index 0) ---
	{
		b, s := newEnv()
		utx, err := b.NewCreateAssetTx(
			"Team Rocket",
			"TR",
			0,
			map[uint32][]verify.State{
				0: {
					&secp256k1fx.MintOutput{
						OutputOwners: *recipientOwner,
					},
					&secp256k1fx.TransferOutput{
						Amt:          1234,
						OutputOwners: utxoOwner,
					},
				},
			},
			opts...,
		)
		emit("x_create_asset", s, utx, err, nil)
	}
	// --- import (AVAX above fee + other asset) ---
	{
		b, s := newEnv()
		utx, err := b.NewImportTx(rsxOtherChainID, recipientOwner, opts...)
		emit("x_import", s, utx, err, nil)
	}
	// --- import (AVAX below fee: local top-up branch) ---
	{
		b, s := newEnv()
		utx, err := b.NewImportTx(rsxSmallChainID, recipientOwner, opts...)
		emit("x_import_avax_lt_fee", s, utx, err, nil)
	}
	// --- export ---
	{
		b, s := newEnv()
		utx, err := b.NewExportTx(rsxOtherChainID, []*avax.TransferableOutput{avaxOutput}, opts...)
		emit("x_export", s, utx, err, nil)
	}

	file := rsxVectorFile{
		Provenance: map[string]string{
			"source":    "avalanchego wallet/chain/x (scratch emitter wallet_avalanche_rs_vectors_x_test.go)",
			"go_commit": os.Getenv("AVAX_RS_GO_COMMIT"),
			"command":   "AVAX_RS_OUT=<path> go test -run TestAvalancheRsXVectors ./wallet/chain/x/",
		},
		Vectors: vectors,
	}
	data, err := json.MarshalIndent(file, "", "  ")
	require.NoError(err)
	require.NoError(os.WriteFile(out, data, 0o600))
}
