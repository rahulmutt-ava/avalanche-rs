// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

// SCRATCH emitter for avalanche-rs golden wallet vectors (M8.25).
// Run:
//
//	AVAX_RS_OUT=/tmp/p_wallet_vectors.json go test -run TestAvalancheRsPVectors ./wallet/chain/p/
//
// Deleted after the vectors are committed to avalanche-rs.

package p

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"os"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/ids"
	"github.com/ava-labs/avalanchego/utils/constants"
	"github.com/ava-labs/avalanchego/utils/crypto/bls"
	"github.com/ava-labs/avalanchego/utils/crypto/bls/signer/localsigner"
	"github.com/ava-labs/avalanchego/utils/crypto/secp256k1"
	"github.com/ava-labs/avalanchego/utils/set"
	"github.com/ava-labs/avalanchego/utils/units"
	"github.com/ava-labs/avalanchego/vms/components/avax"
	"github.com/ava-labs/avalanchego/vms/components/gas"
	"github.com/ava-labs/avalanchego/vms/platformvm/fx"
	"github.com/ava-labs/avalanchego/vms/platformvm/signer"
	"github.com/ava-labs/avalanchego/vms/platformvm/stakeable"
	"github.com/ava-labs/avalanchego/vms/platformvm/txs"
	"github.com/ava-labs/avalanchego/vms/platformvm/warp"
	"github.com/ava-labs/avalanchego/vms/platformvm/warp/message"
	"github.com/ava-labs/avalanchego/vms/platformvm/warp/payload"
	"github.com/ava-labs/avalanchego/vms/secp256k1fx"
	"github.com/ava-labs/avalanchego/wallet/chain/p/builder"
	psigner "github.com/ava-labs/avalanchego/wallet/chain/p/signer"
	"github.com/ava-labs/avalanchego/wallet/chain/p/wallet"
	"github.com/ava-labs/avalanchego/wallet/subnet/primary/common"
	"github.com/ava-labs/avalanchego/wallet/subnet/primary/common/utxotest"
)

type rsVector struct {
	Name        string            `json:"name"`
	Inputs      map[string]string `json:"inputs,omitempty"`
	UnsignedHex string            `json:"unsigned_hex"`
	SignedHex   string            `json:"signed_hex"`
}

type rsVectorFile struct {
	Provenance map[string]string `json:"provenance"`
	Vectors    []rsVector        `json:"vectors"`
}

const (
	rsMinIssuanceTime uint64 = 1_700_000_000
	rsLockTime        uint64 = 1_800_000_000
	rsValidatorEnd    uint64 = 1_750_000_000
)

var (
	rsAvaxAssetID   = ids.Empty.Prefix(1789)
	rsSubnetAssetID = ids.Empty.Prefix(2024)
	rsSubnetID      = ids.Empty.Prefix(7777)
	rsValidationID  = ids.Empty.Prefix(8888)
	rsVMID          = ids.Empty.Prefix(4242)
	rsFxID          = ids.Empty.Prefix(5151)
	rsOtherChainID  = ids.Empty.Prefix(6161)
	rsNodeID        = ids.BuildTestNodeID([]byte{
		0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a,
		0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14,
	})

	rsBLSKeyBytes0 = append(make([]byte, 31), 0x25) // scalar 37
	rsBLSKeyBytes1 = append(make([]byte, 31), 0x26) // scalar 38
)

func rsShortID(b byte) ids.ShortID {
	var s ids.ShortID
	for i := range s {
		s[i] = b
	}
	return s
}

func rsContext() *builder.Context {
	return &builder.Context{
		NetworkID:   constants.UnitTestID,
		AVAXAssetID: rsAvaxAssetID,
		ComplexityWeights: gas.Dimensions{
			gas.Bandwidth: 1,
			gas.DBRead:    10,
			gas.DBWrite:   100,
			gas.Compute:   1000,
		},
		GasPrice: 1,
	}
}

func rsMakeTestUTXOs(utxosKey *secp256k1.PrivateKey) []*avax.UTXO {
	const utxosOffset uint64 = 2024
	utxosAddr := utxosKey.Address()
	return []*avax.UTXO{
		{
			UTXOID: avax.UTXOID{TxID: ids.Empty.Prefix(utxosOffset), OutputIndex: uint32(utxosOffset)},
			Asset:  avax.Asset{ID: rsAvaxAssetID},
			Out: &secp256k1fx.TransferOutput{
				Amt: 2 * units.MilliAvax,
				OutputOwners: secp256k1fx.OutputOwners{
					Threshold: 1,
					Addrs:     []ids.ShortID{utxosAddr},
				},
			},
		},
		{
			UTXOID: avax.UTXOID{TxID: ids.Empty.Prefix(utxosOffset + 1), OutputIndex: uint32(utxosOffset + 1)},
			Asset:  avax.Asset{ID: rsAvaxAssetID},
			Out: &stakeable.LockOut{
				Locktime: rsLockTime,
				TransferableOut: &secp256k1fx.TransferOutput{
					Amt: 3 * units.MilliAvax,
					OutputOwners: secp256k1fx.OutputOwners{
						Threshold: 1,
						Addrs:     []ids.ShortID{utxosAddr},
					},
				},
			},
		},
		{
			UTXOID: avax.UTXOID{TxID: ids.Empty.Prefix(utxosOffset + 2), OutputIndex: uint32(utxosOffset + 2)},
			Asset:  avax.Asset{ID: rsSubnetAssetID},
			Out: &secp256k1fx.TransferOutput{
				Amt: 99 * units.MegaAvax,
				OutputOwners: secp256k1fx.OutputOwners{
					Threshold: 1,
					Addrs:     []ids.ShortID{utxosAddr},
				},
			},
		},
		{
			UTXOID: avax.UTXOID{TxID: ids.Empty.Prefix(utxosOffset + 3), OutputIndex: uint32(utxosOffset + 3)},
			Asset:  avax.Asset{ID: rsAvaxAssetID},
			Out: &stakeable.LockOut{
				Locktime: rsLockTime,
				TransferableOut: &secp256k1fx.TransferOutput{
					Amt: 88 * units.Avax,
					OutputOwners: secp256k1fx.OutputOwners{
						Threshold: 1,
						Addrs:     []ids.ShortID{utxosAddr},
					},
				},
			},
		},
		{
			UTXOID: avax.UTXOID{TxID: ids.Empty.Prefix(utxosOffset + 4), OutputIndex: uint32(utxosOffset + 4)},
			Asset:  avax.Asset{ID: rsAvaxAssetID},
			Out: &secp256k1fx.TransferOutput{
				Amt: 9 * units.Avax,
				OutputOwners: secp256k1fx.OutputOwners{
					Threshold: 1,
					Addrs:     []ids.ShortID{utxosAddr},
				},
			},
		},
	}
}

func rsMakeUTXOs(utxosKey *secp256k1.PrivateKey, amounts ...uint64) []*avax.UTXO {
	utxosOffset := uint64(2024)
	utxosAddr := utxosKey.Address()
	utxos := make([]*avax.UTXO, len(amounts))
	for i, amount := range amounts {
		utxosOffset++
		utxos[i] = &avax.UTXO{
			UTXOID: avax.UTXOID{TxID: ids.Empty.Prefix(utxosOffset), OutputIndex: uint32(utxosOffset)},
			Asset:  avax.Asset{ID: rsAvaxAssetID},
			Out: &secp256k1fx.TransferOutput{
				Amt: amount,
				OutputOwners: secp256k1fx.OutputOwners{
					Threshold: 1,
					Addrs:     []ids.ShortID{utxosAddr},
				},
			},
		}
	}
	return utxos
}

func TestAvalancheRsPVectors(t *testing.T) {
	out := os.Getenv("AVAX_RS_OUT")
	if out == "" {
		t.Skip("AVAX_RS_OUT not set")
	}
	require := require.New(t)
	ctx := context.Background()

	keys := secp256k1.TestKeys()
	subnetAuthKey, utxoKey, validationAuthKey := keys[0], keys[1], keys[2]
	subnetAuthAddr := subnetAuthKey.Address()
	utxoAddr := utxoKey.Address()
	validationAuthAddr := validationAuthKey.Address()

	utxoOwner := secp256k1fx.OutputOwners{Threshold: 1, Addrs: []ids.ShortID{utxoAddr}}
	subnetOwner := &secp256k1fx.OutputOwners{Threshold: 1, Addrs: []ids.ShortID{subnetAuthAddr}}
	validationOwner := &secp256k1fx.OutputOwners{Threshold: 1, Addrs: []ids.ShortID{validationAuthAddr}}
	owners := map[ids.ID]fx.Owner{
		rsSubnetID:     subnetOwner,
		rsValidationID: validationOwner,
	}

	avaxOutput := &avax.TransferableOutput{
		Asset: avax.Asset{ID: rsAvaxAssetID},
		Out: &secp256k1fx.TransferOutput{
			Amt:          7 * units.Avax,
			OutputOwners: utxoOwner,
		},
	}

	kc := secp256k1fx.NewKeychain(keys...)
	addrs := set.Of(utxoAddr, subnetAuthAddr, validationAuthAddr)
	testContext := rsContext()

	defaultUTXOs := rsMakeTestUTXOs(utxoKey)
	stakerUTXOs := rsMakeUTXOs(utxoKey, units.NanoAvax, 9*units.Avax)
	importUTXOs := defaultUTXOs[:1]

	sk0, err := localsigner.FromBytes(rsBLSKeyBytes0)
	require.NoError(err)
	pop0, err := signer.NewProofOfPossession(sk0)
	require.NoError(err)
	sk1, err := localsigner.FromBytes(rsBLSKeyBytes1)
	require.NoError(err)
	pop1, err := signer.NewProofOfPossession(sk1)
	require.NoError(err)

	opts := []common.Option{
		common.WithMinIssuanceTime(rsMinIssuanceTime),
		common.WithChangeOwner(&utxoOwner),
	}

	newEnv := func(utxos []*avax.UTXO, extraChains map[ids.ID][]*avax.UTXO) (builder.Builder, psigner.Signer) {
		sets := map[ids.ID][]*avax.UTXO{constants.PlatformChainID: utxos}
		for chain, u := range extraChains {
			sets[chain] = u
		}
		chainUTXOs := utxotest.NewDeterministicChainUTXOs(t, sets)
		backend := wallet.NewBackend(chainUTXOs, owners)
		return builder.New(addrs, testContext, backend), psigner.New(kc, backend)
	}

	var vectors []rsVector
	emit := func(name string, b builder.Builder, s psigner.Signer, utx txs.UnsignedTx, buildErr error, inputs map[string]string) {
		require.NoError(buildErr, name)
		tx, err := psigner.SignUnsigned(ctx, s, utx)
		require.NoError(err, name)
		unsignedBytes, err := txs.Codec.Marshal(txs.CodecVersion, &utx)
		require.NoError(err, name)
		vectors = append(vectors, rsVector{
			Name:        name,
			Inputs:      inputs,
			UnsignedHex: hex.EncodeToString(unsignedBytes),
			SignedHex:   hex.EncodeToString(tx.Bytes()),
		})
	}

	// --- base ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewBaseTx([]*avax.TransferableOutput{avaxOutput}, opts...)
		emit("p_base", b, s, utx, err, nil)
	}
	// --- base with memo ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewBaseTx([]*avax.TransferableOutput{avaxOutput}, append(opts, common.WithMemo([]byte("memo")))...)
		emit("p_base_memo", b, s, utx, err, nil)
	}
	// --- add_subnet_validator ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewAddSubnetValidatorTx(&txs.SubnetValidator{
			Validator: txs.Validator{NodeID: rsNodeID, End: rsValidatorEnd},
			Subnet:    rsSubnetID,
		}, opts...)
		emit("p_add_subnet_validator", b, s, utx, err, nil)
	}
	// --- remove_subnet_validator ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewRemoveSubnetValidatorTx(rsNodeID, rsSubnetID, opts...)
		emit("p_remove_subnet_validator", b, s, utx, err, nil)
	}
	// --- create_chain ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewCreateChainTx(rsSubnetID, []byte{'a', 'b', 'c'}, rsVMID, []ids.ID{rsFxID}, "dummyChain", opts...)
		emit("p_create_chain", b, s, utx, err, nil)
	}
	// --- create_subnet ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewCreateSubnetTx(subnetOwner, opts...)
		emit("p_create_subnet", b, s, utx, err, nil)
	}
	// --- transfer_subnet_ownership ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewTransferSubnetOwnershipTx(rsSubnetID, subnetOwner, opts...)
		emit("p_transfer_subnet_ownership", b, s, utx, err, nil)
	}
	// --- import ---
	{
		b, s := newEnv(defaultUTXOs, map[ids.ID][]*avax.UTXO{rsOtherChainID: importUTXOs})
		utx, err := b.NewImportTx(rsOtherChainID, subnetOwner, opts...)
		emit("p_import", b, s, utx, err, nil)
	}
	// --- export ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewExportTx(rsOtherChainID, []*avax.TransferableOutput{avaxOutput}, opts...)
		emit("p_export", b, s, utx, err, nil)
	}
	// --- add_permissionless_validator ---
	{
		b, s := newEnv(stakerUTXOs, nil)
		utx, err := b.NewAddPermissionlessValidatorTx(
			&txs.SubnetValidator{
				Validator: txs.Validator{NodeID: rsNodeID, End: rsValidatorEnd, Wght: 2 * units.Avax},
				Subnet:    constants.PrimaryNetworkID,
			},
			pop0,
			rsAvaxAssetID,
			subnetOwner,
			subnetOwner,
			1_000_000,
			opts...,
		)
		emit("p_add_permissionless_validator", b, s, utx, err, map[string]string{
			"bls_pk_0":  hex.EncodeToString(pop0.PublicKey[:]),
			"bls_pop_0": hex.EncodeToString(pop0.ProofOfPossession[:]),
		})
	}
	// --- add_permissionless_delegator ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewAddPermissionlessDelegatorTx(
			&txs.SubnetValidator{
				Validator: txs.Validator{NodeID: rsNodeID, End: rsValidatorEnd, Wght: 2 * units.Avax},
				Subnet:    constants.PrimaryNetworkID,
			},
			rsAvaxAssetID,
			subnetOwner,
			opts...,
		)
		emit("p_add_permissionless_delegator", b, s, utx, err, nil)
	}
	// --- convert_subnet_to_l1 ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		address := make([]byte, 32)
		for i := range address {
			address[i] = 0x5a
		}
		nodeID0 := make([]byte, 20)
		nodeID1 := make([]byte, 20)
		for i := range nodeID0 {
			nodeID0[i] = 0xaa
			nodeID1[i] = 0xbb
		}
		validators := []*txs.ConvertSubnetToL1Validator{
			{
				NodeID:  nodeID0,
				Weight:  0x0102030405060708,
				Balance: units.Avax,
				Signer:  *pop0,
				RemainingBalanceOwner: message.PChainOwner{
					Threshold: 1,
					Addresses: []ids.ShortID{rsShortID(0x11)},
				},
				DeactivationOwner: message.PChainOwner{
					Threshold: 1,
					Addresses: []ids.ShortID{rsShortID(0x22)},
				},
			},
			{
				NodeID:                nodeID1,
				Weight:                0x1112131415161718,
				Balance:               2 * units.Avax,
				Signer:                *pop1,
				RemainingBalanceOwner: message.PChainOwner{},
				DeactivationOwner:     message.PChainOwner{},
			},
		}
		utx, err := b.NewConvertSubnetToL1Tx(rsSubnetID, rsOtherChainID, address, validators, opts...)
		emit("p_convert_subnet_to_l1", b, s, utx, err, map[string]string{
			"bls_pk_0":  hex.EncodeToString(pop0.PublicKey[:]),
			"bls_pop_0": hex.EncodeToString(pop0.ProofOfPossession[:]),
			"bls_pk_1":  hex.EncodeToString(pop1.PublicKey[:]),
			"bls_pop_1": hex.EncodeToString(pop1.ProofOfPossession[:]),
		})
	}
	// --- register_l1_validator ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		addressedCallPayload, err := message.NewRegisterL1Validator(
			rsSubnetID,
			rsNodeID,
			pop0.PublicKey,
			1731005097,
			message.PChainOwner{Threshold: 1, Addresses: []ids.ShortID{rsShortID(0x33)}},
			message.PChainOwner{Threshold: 1, Addresses: []ids.ShortID{rsShortID(0x44)}},
			7905001371,
		)
		require.NoError(err)
		sourceAddr := make([]byte, 20)
		for i := range sourceAddr {
			sourceAddr[i] = 0x66
		}
		addressedCall, err := payload.NewAddressedCall(sourceAddr, addressedCallPayload.Bytes())
		require.NoError(err)
		unsignedWarp, err := warp.NewUnsignedMessage(constants.UnitTestID, rsOtherChainID, addressedCall.Bytes())
		require.NoError(err)
		sig, err := sk0.Sign(unsignedWarp.Bytes())
		require.NoError(err)
		var sigBytes [bls.SignatureLen]byte
		copy(sigBytes[:], bls.SignatureToBytes(sig))
		warpMsg, err := warp.NewMessage(unsignedWarp, &warp.BitSetSignature{
			Signers:   set.NewBits(0).Bytes(),
			Signature: sigBytes,
		})
		require.NoError(err)

		utx, buildErr := b.NewRegisterL1ValidatorTx(units.Avax, pop0.ProofOfPossession, warpMsg.Bytes(), opts...)
		emit("p_register_l1_validator", b, s, utx, buildErr, map[string]string{
			"warp_message": hex.EncodeToString(warpMsg.Bytes()),
			"bls_pop_0":    hex.EncodeToString(pop0.ProofOfPossession[:]),
		})
	}
	// --- set_l1_validator_weight ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		addressedCallPayload, err := message.NewL1ValidatorWeight(rsValidationID, 1, 7905001371)
		require.NoError(err)
		sourceAddr := make([]byte, 20)
		for i := range sourceAddr {
			sourceAddr[i] = 0x77
		}
		addressedCall, err := payload.NewAddressedCall(sourceAddr, addressedCallPayload.Bytes())
		require.NoError(err)
		unsignedWarp, err := warp.NewUnsignedMessage(constants.UnitTestID, rsOtherChainID, addressedCall.Bytes())
		require.NoError(err)
		sig, err := sk0.Sign(unsignedWarp.Bytes())
		require.NoError(err)
		var sigBytes [bls.SignatureLen]byte
		copy(sigBytes[:], bls.SignatureToBytes(sig))
		warpMsg, err := warp.NewMessage(unsignedWarp, &warp.BitSetSignature{
			Signers:   set.NewBits(0).Bytes(),
			Signature: sigBytes,
		})
		require.NoError(err)

		utx, buildErr := b.NewSetL1ValidatorWeightTx(warpMsg.Bytes(), opts...)
		emit("p_set_l1_validator_weight", b, s, utx, buildErr, map[string]string{
			"warp_message": hex.EncodeToString(warpMsg.Bytes()),
		})
	}
	// --- increase_l1_validator_balance ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewIncreaseL1ValidatorBalanceTx(rsValidationID, units.Avax, opts...)
		emit("p_increase_l1_validator_balance", b, s, utx, err, nil)
	}
	// --- disable_l1_validator ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewDisableL1ValidatorTx(rsValidationID, opts...)
		emit("p_disable_l1_validator", b, s, utx, err, nil)
	}
	// --- add_auto_renewed_validator (ACP-236) ---
	{
		b, s := newEnv(stakerUTXOs, nil)
		utx, err := b.NewAddAutoRenewedValidatorTx(
			rsNodeID,
			2*units.Avax,
			pop0,
			subnetOwner,
			subnetOwner,
			validationOwner,
			1_000_000,
			500_000,
			7*24*time.Hour,
			opts...,
		)
		emit("p_add_auto_renewed_validator", b, s, utx, err, map[string]string{
			"bls_pk_0":  hex.EncodeToString(pop0.PublicKey[:]),
			"bls_pop_0": hex.EncodeToString(pop0.ProofOfPossession[:]),
		})
	}
	// --- set_auto_renewed_validator_config (ACP-236) ---
	{
		b, s := newEnv(defaultUTXOs, nil)
		utx, err := b.NewSetAutoRenewedValidatorConfigTx(rsValidationID, 750_000, 14*24*time.Hour, opts...)
		emit("p_set_auto_renewed_validator_config", b, s, utx, err, nil)
	}

	file := rsVectorFile{
		Provenance: map[string]string{
			"source":    "avalanchego wallet/chain/p (scratch emitter wallet_avalanche_rs_vectors_p_test.go)",
			"go_commit": os.Getenv("AVAX_RS_GO_COMMIT"),
			"command":   "AVAX_RS_OUT=<path> go test -run TestAvalancheRsPVectors ./wallet/chain/p/",
		},
		Vectors: vectors,
	}
	data, err := json.MarshalIndent(file, "", "  ")
	require.NoError(err)
	require.NoError(os.WriteFile(out, data, 0o600))
}
