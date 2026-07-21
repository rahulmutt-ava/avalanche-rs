// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package p2p

// p2p SDK wire-frame golden emitter for the avalanche-rs cchain-tx-gossip
// Task 15 byte-goldens (crates/ava-p2p/tests/wire_goldens.rs).
//
// This is the LIVE Go oracle: for three fixed `proto/pb/sdk` messages
// (PushGossip, PullGossipRequest, PullGossipResponse) it prefixes the
// proto-marshaled bytes with `ProtocolPrefix(0)` via `PrefixMessage` — exactly
// the framing `network/p2p.Network`'s gossip/request paths use on the wire —
// and writes each resulting frame to its own `.bin` file. The Rust side builds
// the identical frame from the same fixed inputs (`network::protocol_prefix` +
// prost `encode_to_vec`) and byte-compares against the committed golden, then
// decodes the golden back and asserts the fields round-trip.
//
// Gated behind `P2P_SDK_EMIT_WIRE_GOLDENS=<output-dir>` so a normal `go test`
// run never executes it; re-freeze the corpus with:
//
//	P2P_SDK_EMIT_WIRE_GOLDENS=/abs/tests/vectors/p2p_sdk \
//	  go test ./network/p2p/ -run TestEmitP2pSdkWireGoldens -count=1 -v
//
// The committed source-of-truth copy of this file lives in the avalanche-rs
// repo under tests/differential/go-oracle/; this copy is dropped into the
// avalanchego checkout's `network/p2p/` package to run (it only needs that
// package's own exported `ProtocolPrefix`/`PrefixMessage`, so it compiles as a
// same-package test with no unexported harness).

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"

	"github.com/ava-labs/avalanchego/proto/pb/sdk"
)

// TestEmitP2pSdkWireGoldens writes the three fixed-input prefixed-frame
// goldens consumed by `crates/ava-p2p/tests/wire_goldens.rs`.
func TestEmitP2pSdkWireGoldens(t *testing.T) {
	outDir := os.Getenv("P2P_SDK_EMIT_WIRE_GOLDENS")
	if outDir == "" {
		t.Skip("set P2P_SDK_EMIT_WIRE_GOLDENS=<output-dir> to emit the p2p_sdk wire goldens")
	}
	require.NoError(t, os.MkdirAll(outDir, 0o755), "MkdirAll(outDir)")

	// Fixed 32-byte salt: 0x01..0x20.
	salt := make([]byte, 32)
	for i := range salt {
		salt[i] = byte(i + 1)
	}
	// Fixed 8-byte filter: 0xF0..0xF7.
	filter := make([]byte, 8)
	for i := range filter {
		filter[i] = byte(0xF0 + i)
	}

	cases := []struct {
		file string
		msg  proto.Message
	}{
		{
			file: "push_gossip_frame.bin",
			msg:  &sdk.PushGossip{Gossip: [][]byte{{0xDE, 0xAD}, {0xBE, 0xEF}}},
		},
		{
			file: "pull_gossip_request.bin",
			msg:  &sdk.PullGossipRequest{Salt: salt, Filter: filter},
		},
		{
			file: "pull_gossip_response.bin",
			msg:  &sdk.PullGossipResponse{Gossip: [][]byte{{0xCA, 0xFE}}},
		},
	}

	prefix := ProtocolPrefix(0)
	for _, c := range cases {
		marshaled, err := proto.Marshal(c.msg)
		require.NoErrorf(t, err, "proto.Marshal(%T)", c.msg)
		frame := PrefixMessage(prefix, marshaled)
		path := filepath.Join(outDir, c.file)
		require.NoError(t, os.WriteFile(path, frame, 0o644), "WriteFile(%s)", path)
		t.Logf("wrote %s (%d bytes)", path, len(frame))
	}
}
