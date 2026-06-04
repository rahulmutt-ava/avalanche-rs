# tests/vectors/ids

Golden string-encoding vectors for ids/addresses. Produced by
`tools/extract-vectors` (M0.2). Owning spec: `specs/03-core-primitives.md`
§1.1, §3.2, §8 item 5; `specs/15` §4.4.

> **Committed** (avalanchego `fb174e8`; see `../manifest.json`).

| File | Schema | Consumed by |
|---|---|---|
| `cb58.json` | `[{ "kind": "id\|short_id\|node_id", "bytes_hex": "..", "string": ".." }]` | `crates/ava-types/tests/golden_cb58.rs` (M0.6) |
| `cb58_raw.json` | `[{ "bytes_hex": "..", "cb58": ".." }]` (+ bad-checksum / too-short cases) | `crates/ava-utils/tests/golden_cb58_codec.rs` (M0.11) |

Includes Mainnet/Fuji known values; `node_id` entries carry the `NodeID-` prefix.
