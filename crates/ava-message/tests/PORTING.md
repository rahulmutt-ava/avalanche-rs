# ava-message — Go test porting matrix

Tracks every relevant Go test in `avalanchego/message` + `network/peer`
(framing) against its Rust counterpart (specs/02 §10.1). "done" = no `wip` rows
and every non-`na` Go test maps to a passing Rust test.

Scope for M2 Wave A: the codec/framing/builder surface (`05` §1.1/§1.2/§1.3/§2,
`15` §3.1). The peer state-machine tests (`network/peer/peer_test.go`, handshake
disconnect cases) belong to `ava-network` (M2.14–M2.17) and are tracked there.

## `message/` package

| Go test (file) | Subject | Rust counterpart | Status |
|---|---|---|---|
| `TestMessage` (`messages_test.go`) | per-op marshal/unmarshal round-trip, compression on/off, bytes-saved | `codec_roundtrip::*`, `prop_frame::frame_roundtrip`, `golden::message_frames` | ported |
| `TestMessage` byte-exactness (per-op wire bytes) | each op's wire frame matches Go | `golden::message_frames` (handshake, ping, pong, get_peerlist, peerlist, get, chits, app_request vectors) | ported |
| `TestInboundMessageToString` (`messages_test.go`) | `Op.String()` names | `ops_table::op_values_and_strings_match_go` | ported |
| `TestEmptyInboundMessage` / `TestNilInboundMessage` | empty/nil oneof → error | `codec_roundtrip::*` (unset oneof → `UnknownOp`), `prop_fuzz_smoke` | ported |
| `ops.go` `Op` iota + `UnrequestedOps` + `FailedToResponseOps` | classification sets | `ops_table::*` | ported |
| `ToOp` / `Unwrap` (`ops.go`) | oneof variant → Op | `ops_table` (`Op::of`) | ported |
| zstd recursive packing (`messages.go` compression cases) | compress→decompress decode-equivalence (R4) | `codec_roundtrip::marshal_unmarshal_zstd_roundtrip` | ported |
| `outbound_msg_builder.go` Handshake/Ping/Pong/GetPeerList/PeerList | builder field wiring + per-op compression | `builder::*`, `golden::message_frames` | ported |
| `outbound_msg_builder.go` consensus/state-sync/app/simplex builders | bulk-op builders | — | na (deferred to consuming engine milestones; see crate docs / report FINDINGS) |
| `creator.go` `NewCreator` | Creator assembly | `builder::Creator` | ported |

## `network/peer/` framing

| Go test (file) | Subject | Rust counterpart | Status |
|---|---|---|---|
| `TestWriteMsgLen` / `TestReadMsgLen` (`msg_length_test.go`) | 4-byte BE length prefix + 2 MiB cap | `frame::*` | ported |

## Fuzz (specs/02 §8)

| Target | Subject | Rust counterpart | Status |
|---|---|---|---|
| arbitrary wire-frame parse (never panic / over-read) | `MsgBuilder::unmarshal` | `fuzz/fuzz_targets/decode_never_overreads.rs` (nightly) + `prop_fuzz_smoke::fuzz_decode_*` (stable) | ported |

## Notes

- The `Handshake.ip_addr` field uses the 16-byte As16 form (IPv4 → IPv4-mapped
  IPv6). Go's `outbound_msg_builder.go` calls `Addr().AsSlice()`; the golden
  vectors were captured with a 16-byte IP, so the encodings coincide. A node that
  advertises a bare-4-byte IPv4 would diverge — revisit when `ava-network` wires
  the real `MyIPPort` (see report FINDINGS).
- `GetPeerList`/`PeerList` are sent with the Creator's **default compression**
  (zstd), not uncompressed — matching Go `outbound_msg_builder.go`
  (`b.compressionType`), contrary to a literal reading of plan/spec §1.3. The
  golden vectors capture the **uncompressed** form (byte-exact), and the
  round-trip/golden tests force `Compression::None` for them.
