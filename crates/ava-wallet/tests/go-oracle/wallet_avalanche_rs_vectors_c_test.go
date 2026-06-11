// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

// SCRATCH emitter for avalanche-rs golden wallet vectors (M8.26).
// Run from an avalanchego checkout (copy into wallet/chain/c/):
//
//	AVAX_RS_OUT=/tmp/c.json go test -run TestAvalancheRsCVectors ./wallet/chain/c/
//
// Delete from the Go tree after the vectors are committed to avalanche-rs
// (crates/ava-wallet/tests/vectors/wallet/c.json).

package c

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"math/big"
	"os"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/graft/coreth/plugin/evm/atomic"
	"github.com/ava-labs/avalanchego/ids"
	"github.com/ava-labs/avalanchego/utils/constants"
	"github.com/ava-labs/avalanchego/utils/crypto/secp256k1"
	"github.com/ava-labs/avalanchego/utils/set"
	"github.com/ava-labs/avalanchego/utils/units"
	"github.com/ava-labs/avalanchego/vms/components/avax"
	"github.com/ava-labs/avalanchego/vms/secp256k1fx"
	"github.com/ava-labs/avalanchego/wallet/subnet/primary/common"
	"github.com/ava-labs/avalanchego/wallet/subnet/primary/common/utxotest"

	ethcommon "github.com/ava-labs/libevm/common"
)

type rscVector struct {
	Name        string            `json:"name"`
	Inputs      map[string]string `json:"inputs,omitempty"`
	UnsignedHex string            `json:"unsigned_hex"`
	SignedHex   string            `json:"signed_hex"`
}

type rscVectorFile struct {
	Provenance map[string]string `json:"provenance"`
	Vectors    []rscVector       `json:"vectors"`
}

const (
	rscMinIssuanceTime uint64 = 1_700_000_000
	rscNonce           uint64 = 7
)

var (
	rscAvaxAssetID  = ids.Empty.Prefix(1789)
	rscOtherAssetID = ids.Empty.Prefix(2024)
	rscChainID      = ids.Empty.Prefix(2025)
	rscXChainID     = ids.Empty.Prefix(2021)
	// 25 gWei.
	rscBaseFee = big.NewInt(25_000_000_000)
	// 5 AVAX in wei.
	rscBalanceWei = new(big.Int).Mul(big.NewInt(5_000_000_000), big.NewInt(1_000_000_000))
)

func rscContext() *Context {
	return &Context{
		NetworkID:    constants.UnitTestID,
		BlockchainID: rscChainID,
		AVAXAssetID:  rscAvaxAssetID,
	}
}

func rscSecpUTXO(prefix uint64, assetID ids.ID, amt uint64, addr ids.ShortID) *avax.UTXO {
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

func TestAvalancheRsCVectors(t *testing.T) {
	out := os.Getenv("AVAX_RS_OUT")
	if out == "" {
		t.Skip("AVAX_RS_OUT not set")
	}
	require := require.New(t)
	ctx := context.Background()

	keys := secp256k1.TestKeys()
	recipientKey, utxoKey := keys[0], keys[1]
	utxoAddr := utxoKey.Address()
	recipientEthAddr := recipientKey.EthAddress()
	utxoEthAddr := utxoKey.EthAddress()

	// Importable UTXOs exported from the X-chain: two AVAX + one non-AVAX
	// (which must be skipped — only AVAX is importable to the C-chain).
	xChainUTXOs := []*avax.UTXO{
		rscSecpUTXO(3024, rscAvaxAssetID, 2*units.MilliAvax, utxoAddr),
		rscSecpUTXO(3025, rscOtherAssetID, 5*units.Avax, utxoAddr),
		rscSecpUTXO(3026, rscAvaxAssetID, 9*units.Avax, utxoAddr),
	}
	// Importable UTXO exported from the P-chain.
	pChainUTXOs := []*avax.UTXO{
		rscSecpUTXO(5024, rscAvaxAssetID, 3*units.MilliAvax, utxoAddr),
	}

	kc := secp256k1fx.NewKeychain(keys...)
	testContext := rscContext()

	opts := []common.Option{
		common.WithMinIssuanceTime(rscMinIssuanceTime),
	}

	newEnv := func() (Builder, Signer) {
		sets := map[ids.ID][]*avax.UTXO{
			rscXChainID:               xChainUTXOs,
			constants.PlatformChainID: pChainUTXOs,
		}
		deterministicUTXOs := utxotest.NewDeterministicChainUTXOs(t, sets)
		backend := NewBackend(deterministicUTXOs, map[ethcommon.Address]*Account{
			utxoEthAddr: {
				Balance: new(big.Int).Set(rscBalanceWei),
				Nonce:   rscNonce,
			},
		})
		// Only the funded eth address: the Go backend errors (ErrNotFound) on
		// accounts it does not know, and a multi-address set iterates randomly.
		b := NewBuilder(
			kc.Addresses(),
			set.Of(utxoEthAddr),
			testContext,
			backend,
		)
		return b, NewSigner(kc, kc, backend)
	}

	sharedInputs := map[string]string{
		"utxo_eth_addr":      hex.EncodeToString(utxoEthAddr[:]),
		"recipient_eth_addr": hex.EncodeToString(recipientEthAddr[:]),
		"base_fee_wei":       rscBaseFee.String(),
		"balance_wei":        rscBalanceWei.String(),
	}

	var vectors []rscVector
	emit := func(name string, s Signer, utx atomic.UnsignedAtomicTx, buildErr error) {
		require.NoError(buildErr, name)
		tx, err := SignUnsignedAtomic(ctx, s, utx)
		require.NoError(err, name)
		unsignedBytes, err := atomic.Codec.Marshal(atomic.CodecVersion, &utx)
		require.NoError(err, name)
		vectors = append(vectors, rscVector{
			Name:        name,
			Inputs:      sharedInputs,
			UnsignedHex: hex.EncodeToString(unsignedBytes),
			SignedHex:   hex.EncodeToString(tx.SignedBytes()),
		})
	}

	// --- import X -> C ---
	{
		b, s := newEnv()
		utx, err := b.NewImportTx(rscXChainID, recipientEthAddr, rscBaseFee, opts...)
		emit("c_import_x", s, utx, err)
	}
	// --- import P -> C ---
	{
		b, s := newEnv()
		utx, err := b.NewImportTx(constants.PlatformChainID, recipientEthAddr, rscBaseFee, opts...)
		emit("c_import_p", s, utx, err)
	}
	// --- export C -> X ---
	{
		b, s := newEnv()
		utx, err := b.NewExportTx(
			rscXChainID,
			[]*secp256k1fx.TransferOutput{{
				Amt: units.Avax,
				OutputOwners: secp256k1fx.OutputOwners{
					Threshold: 1,
					Addrs:     []ids.ShortID{recipientKey.Address()},
				},
			}},
			rscBaseFee,
			opts...,
		)
		emit("c_export_x", s, utx, err)
	}

	file := rscVectorFile{
		Provenance: map[string]string{
			"source":    "avalanchego wallet/chain/c (scratch emitter wallet_avalanche_rs_vectors_c_test.go)",
			"go_commit": os.Getenv("AVAX_RS_GO_COMMIT"),
			"command":   "AVAX_RS_OUT=<path> go test -run TestAvalancheRsCVectors ./wallet/chain/c/",
		},
		Vectors: vectors,
	}
	data, err := json.MarshalIndent(file, "", "  ")
	require.NoError(err)
	require.NoError(os.WriteFile(out, data, 0o600))
}
