# tests/vectors/p2p_sdk

Byte-goldens proving Rust's varint-prefixed gossip frames (Go
`network/p2p.PrefixMessage(network/p2p.ProtocolPrefix(handlerID), proto.Marshal(msg))`)
match Go exactly, for the three `proto/pb/sdk` gossip message types the
C-Chain tx-gossip wiring uses (cchain-tx-gossip Task 15). Owning spec:
`specs/07-networking-and-p2p.md` (gossip framing), `specs/16-p2p-sdk-gossip.md`.

Unlike the rest of `tests/vectors/` (produced by `tools/extract-vectors/extract.go`
against the pin recorded in `../manifest.json`), this surface is a **live Go
oracle** — same pattern as `tests/vectors/saevm/{recovery,streaming}_differential/`
— per `tests/differential/go-oracle/README.md`.

- **Emitter (source-of-truth copy)**: `tests/differential/go-oracle/p2p_sdk_wire_emitter_test.go`
  (`TestEmitP2pSdkWireGoldens`, gated on `P2P_SDK_EMIT_WIRE_GOLDENS`). It is a
  same-package (`network/p2p`) test — it only needs that package's own
  exported `ProtocolPrefix`/`PrefixMessage`, no unexported test harness — so
  it's dropped straight into an avalanchego checkout's `network/p2p/` to run.
- **Consumer**: `crates/ava-p2p/tests/wire_goldens.rs` — for each golden,
  builds the identical frame from the same fixed inputs
  (`network::protocol_prefix` + prost `encode_to_vec`) and byte-compares
  against `include_bytes!` of the golden (encode leg), then parses the
  golden's prefix (`network::parse_prefix`) and prost-decodes the payload,
  asserting the fields equal the fixed inputs (decode leg).
- **Provenance**: emitted from avalanchego @
  `5c4d318161d2c34a14a635632738b739704aef7b` (`rpcchainvm=45`), verified via
  `./scripts/check_oracle_binary.sh` before capture. This is a **different**
  pin than `../manifest.json`'s `avalanchego_revision` (that one tracks the
  `tools/extract-vectors` corpus; this surface is captured independently, like
  the saevm differential corpora).

Fixed inputs (all three frames use `ProtocolPrefix(0)`):

| File | Message | Fixed value |
|------|---------|-------------|
| `push_gossip_frame.bin` | `sdk.PushGossip` | `Gossip: [][]byte{{0xDE,0xAD},{0xBE,0xEF}}` |
| `pull_gossip_request.bin` | `sdk.PullGossipRequest` | `Salt: 0x01..0x20` (32 bytes), `Filter: 0xF0..0xF7` (8 bytes) |
| `pull_gossip_response.bin` | `sdk.PullGossipResponse` | `Gossip: [][]byte{{0xCA,0xFE}}` |

## Re-freezing the corpus (live mode)

```sh
./scripts/check_oracle_binary.sh   # must print OK before capture

AVALANCHEGO_DIR=${AVALANCHEGO_DIR:-../avalanchego}
cp tests/differential/go-oracle/p2p_sdk_wire_emitter_test.go \
   "$AVALANCHEGO_DIR/network/p2p/"

cd "$AVALANCHEGO_DIR"
P2P_SDK_EMIT_WIRE_GOLDENS="$OLDPWD/tests/vectors/p2p_sdk" \
  go test ./network/p2p/ -run TestEmitP2pSdkWireGoldens -count=1 -v

rm network/p2p/p2p_sdk_wire_emitter_test.go   # keep the checkout clean; the
                                                # in-repo copy is the durable one
```

Then re-run the Rust per-PR test to confirm parity:

```sh
cargo nextest run -p ava-p2p --test wire_goldens
```

Without `P2P_SDK_EMIT_WIRE_GOLDENS` set, `TestEmitP2pSdkWireGoldens` is
skipped, so the emitter never runs during a normal `go test`.
