// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package message

// message envelope wire-frame golden emitter for the avalanche-rs
// cchain-tx-gossip Task 16 live-debugging probe.
//
// This is the LIVE Go oracle: it builds the exact outbound `p2p.Message`
// envelope bytes (`message.Creator.AppGossip`/`AppRequest`, i.e. what
// `OutboundMessage.Bytes` holds — the plain `proto.Marshal` output with
// compression disabled, matching `messages.go`'s `createOutbound`/`marshal`)
// for a fixed chainID and payload, and writes each resulting frame to its own
// `.bin` file. The Rust side builds the identical envelope from the same
// fixed inputs (`ava_message::codec::MsgBuilder::create_outbound` with
// `Compression::None`, mirroring `crates/ava-engine/src/networking/sender.rs`'s
// `dispatch`/`gossip` calls) and byte-compares against the committed golden.
//
// Probe 1 (cchain-tx-gossip task 16): decisive check of whether the Rust→Go
// AppGossip wire envelope encoding itself has a bug, isolated from framing/
// compression/network concerns. AppRequest is included as a known-good
// baseline (Rust AppRequest/AppResponse already interop with real Go peers
// live) — if AppRequest matches and AppGossip doesn't, the delta IS the bug;
// if both match, envelope encoding is exonerated.
//
// Gated behind `MESSAGE_EMIT_ENVELOPE_GOLDENS=<output-dir>` so a normal
// `go test` run never executes it; re-freeze the corpus with:
//
//	MESSAGE_EMIT_ENVELOPE_GOLDENS=/abs/tests/vectors/message_envelope \
//	  go test ./message/ -run TestEmitMessageEnvelopeGoldens -count=1 -v
//
// The committed source-of-truth copy of this file lives in the avalanche-rs
// repo under tests/differential/go-oracle/; this copy is dropped into the
// avalanchego checkout's `message/` package to run (it only needs that
// package's own exported `NewCreator`, so it compiles as a same-package test
// with no unexported harness).

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/ids"
	"github.com/ava-labs/avalanchego/utils/compression"
)

// TestEmitMessageEnvelopeGoldens writes the fixed-input `p2p.Message` envelope
// goldens (AppGossip + AppRequest baseline) consumed by
// `crates/ava-message/tests/envelope_goldens.rs`.
func TestEmitMessageEnvelopeGoldens(t *testing.T) {
	outDir := os.Getenv("MESSAGE_EMIT_ENVELOPE_GOLDENS")
	if outDir == "" {
		t.Skip("set MESSAGE_EMIT_ENVELOPE_GOLDENS=<output-dir> to emit the message envelope goldens")
	}
	require.NoError(t, os.MkdirAll(outDir, 0o755), "MkdirAll(outDir)")

	// Fixed 32-byte chain id: 0x01..0x20 (matches the p2p_sdk emitter's salt
	// convention, cchain-tx-gossip Task 15).
	var chainID ids.ID
	for i := range chainID {
		chainID[i] = byte(i + 1)
	}

	// The T15 push_gossip_frame bytes: the varint-handler-id-prefixed
	// PushGossip SDK frame this repo already committed at
	// tests/vectors/p2p_sdk/push_gossip_frame.bin — i.e. exactly the bytes
	// `ava_p2p::client::Client::app_gossip` hands to `AppSender::send_app_gossip`
	// as `app_bytes` in production.
	appBytes := []byte{
		0x00, // varint handler id 0 (TxGossipHandlerID)
	}
	// PushGossip{Gossip: [][]byte{{0xDE,0xAD},{0xBE,0xEF}}} proto-marshaled,
	// per the Task 15 emitter — reproduced verbatim here rather than reading
	// the committed .bin so this test has no cross-package file dependency.
	appBytes = append(appBytes, 0x0a, 0x02, 0xde, 0xad, 0x0a, 0x02, 0xbe, 0xef)

	creator, err := NewCreator(prometheus.NewRegistry(), compression.TypeNone, 10*time.Second)
	require.NoError(t, err, "NewCreator")

	gossipMsg, err := creator.AppGossip(chainID, appBytes)
	require.NoError(t, err, "Creator.AppGossip")
	requireWriteEnvelope(t, filepath.Join(outDir, "app_gossip_envelope.bin"), gossipMsg.Bytes)

	requestMsg, err := creator.AppRequest(chainID, 1, time.Second, appBytes)
	require.NoError(t, err, "Creator.AppRequest")
	requireWriteEnvelope(t, filepath.Join(outDir, "app_request_envelope.bin"), requestMsg.Bytes)
}

func requireWriteEnvelope(t *testing.T, path string, bytes []byte) {
	t.Helper()
	require.NoError(t, os.WriteFile(path, bytes, 0o644), "WriteFile(%s)", path)
	t.Logf("wrote %s (%d bytes)", path, len(bytes))
}
