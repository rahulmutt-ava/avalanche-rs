// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

// M6.29 — Go-EXECUTED GasUsed oracle for the M6.14 atomic_txs.json corpus
// (1-input ImportTx + 1-input ExportTx, both with one secp256k1 signature).
//
// Drop this file into `graft/coreth/plugin/evm/atomic/` in the avalanchego
// checkout and run:
//
//	AVAX_RS_EMIT_ATOMIC_GAS=1 go test -run TestEmitAtomicTxGasUsed -v ./plugin/evm/atomic/
//
// It parses the exact unsigned-tx interface bytes already committed in
// `crates/ava-evm/tests/vectors/cchain/atomic/atomic_txs.json`
// (`unsigned_import_tx.interface_codec_hex` / `unsigned_export_tx.interface_codec_hex`),
// signs each with one key (signature length is fixed at 65 bytes, so GasUsed is
// key-independent), and prints `GasUsed(fixedFee)` for both fixedFee modes plus
// the unsigned/signed byte lengths. The emitted JSON freezes:
//
//   - coreth `Metadata.Bytes()` returns the UNSIGNED bytes (metadata.go:30) —
//     the Go method name is misleading; `SignedBytes()` is the full signed tx.
//   - `GasUsed(fixedFee=true)` = len(unsignedBytes)*TxBytesGas
//     + per-input cost (len(SigIndices)*CostPerSignature) + ap5.AtomicTxIntrinsicGas
//     (import_tx.go:136, export_tx.go:134, tx.go:340 calcBytesCost).
//
// Output is baked into `crates/ava-evm/tests/vectors/cchain/atomic/atomic_txs.json`
// (`gas_used` key) and asserted by `crates/ava-evm/tests/atomic_mempool.rs`.
package atomic_test

import (
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/graft/coreth/plugin/evm/atomic"
	"github.com/ava-labs/avalanchego/utils/crypto/secp256k1"
)

const (
	// unsigned_import_tx.interface_codec_hex from atomic_txs.json (M6.14).
	importIfaceHex = "000000000000000000011111111111111111111111111111111111111111111111111111111111111111222222222222222222222222222222222222222222222222222222222222222200000001444444444444444444444444444444444444444444444444444444444444444400000001aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa00000005000000000000138800000001000000000000000101010101010101010101010101010101010101010000000000001387aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	// unsigned_export_tx.interface_codec_hex from atomic_txs.json (M6.14).
	exportIfaceHex = "00000000000100000001111111111111111111111111111111111111111111111111111111111111111133333333333333333333333333333333333333333333333333333333333333330000000102020202020202020202020202020202020202020000000000000bb8aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa000000000000000700000001aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa000000070000000000000bb8000000000000000000000001000000010505050505050505050505050505050505050505"
)

func TestEmitAtomicTxGasUsed(t *testing.T) {
	if os.Getenv("AVAX_RS_EMIT_ATOMIC_GAS") == "" {
		t.Skip("set AVAX_RS_EMIT_ATOMIC_GAS=1 to emit")
	}

	key, err := secp256k1.NewPrivateKey()
	require.NoError(t, err)

	out := map[string]any{}
	for name, ifaceHex := range map[string]string{
		"import": importIfaceHex,
		"export": exportIfaceHex,
	} {
		raw, err := hex.DecodeString(ifaceHex)
		require.NoError(t, err)

		var utx atomic.UnsignedAtomicTx
		version, err := atomic.Codec.Unmarshal(raw, &utx)
		require.NoError(t, err)
		require.Zero(t, version)

		tx := &atomic.Tx{UnsignedAtomicTx: utx}
		require.NoError(t, tx.Sign(atomic.Codec, [][]*secp256k1.PrivateKey{{key}}))

		gasFixedFee, err := tx.GasUsed(true)
		require.NoError(t, err)
		gasNoFixedFee, err := tx.GasUsed(false)
		require.NoError(t, err)

		// Metadata.Bytes() == the UNSIGNED bytes we parsed (metadata.go:30).
		require.Equal(t, raw, tx.Bytes())

		out[name] = map[string]any{
			"unsigned_bytes_len":    len(tx.Bytes()),
			"signed_bytes_len":      len(tx.SignedBytes()),
			"gas_used_fixed_fee":    gasFixedFee,
			"gas_used_no_fixed_fee": gasNoFixedFee,
		}
	}

	j, err := json.MarshalIndent(out, "", "  ")
	require.NoError(t, err)
	fmt.Println(string(j))
}
