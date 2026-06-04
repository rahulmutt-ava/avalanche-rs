# tests/vectors/codec

Golden wire-byte vectors for the hand-written linear codec, per registered-type
family. Produced by `tools/extract-vectors` (M0.2); consumed by
`crates/ava-codec/tests/golden_codec.rs` (M0.16, `golden::codec_all_types`).
Owning spec: `specs/03-core-primitives.md` §8 item 1, `specs/15` §4.1, §6.

> No `.json` committed yet — extracted out of band (see `../manifest.json`).

Schema (one file per family, or one combined file of cases):

```json
[{ "name": "fixed_array | vec_u8 | vec_struct | interface_typeid | map | nested",
   "version": 0,
   "value_desc": "human description of the value",
   "expected_hex": "00 00 ..  (incl. the 2-byte version prefix)" }]
```

Negative cases to include: trailing bytes (`ExtraSpace`), oversize slice
(`MaxSliceLenExceeded`), bad bool, unsorted map keys, unknown typeID, unknown
version. A separate typeID table (linearcodec) is dumped for `linearcodec`
registration-order assertions (M0.16).
