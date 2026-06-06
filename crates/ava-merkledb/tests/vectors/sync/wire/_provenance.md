# sync wire vectors provenance

`proof_frames.json` holds **real Go-marshaled** `proto/pb/sync` wire frames for
`ProofRequest` / `ProofResponse` (incl. bare `MaybeBytes`), used by
`golden::sync_proof_wire` (`tests/sync_roundtrip.rs`) to prove byte-exact parity
with the Go node's state-sync wire format (`specs/15` §3.10, `19` §4).

Each frame is `proto.MarshalOptions{Deterministic:true}.Marshal(msg)` hex-encoded.

| Field | Value |
|-------|-------|
| Source repo | `github.com/ava-labs/avalanchego` |
| Source rev | `fb174e8925` |
| Source path | `database/merkle/sync/` (proto `proto/pb/sync`) |
| Extracted by | scratch in-package `_test.go` (`TestExtractSyncWireVectors`), since deleted |

Vectors:
- `range_proof_request_bounded` — both bounds present (`MaybeBytes` Some).
- `range_proof_request_unbounded_start` — `start_key` Nothing (nil), `end_key` Some.
- `range_proof_request_empty_start_value` — `start_key` Some(empty bytes) (present-but-empty).
- `change_proof_request_bounded` — both root hashes + both bounds.
- `change_proof_request_unbounded` — both bounds Nothing.
- `proof_response_range` / `proof_response_change` — the `oneof response` arms.
- `maybe_bytes_present` / `maybe_bytes_present_empty` — bare `MaybeBytes` framing
  (the latter marshals to **empty bytes** because its only field is empty bytes,
  which proto3 omits — presence of the message itself is carried by the parent).
