<!--
Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
See the file LICENSE for licensing terms.
-->

# `ava-network` — gRPC / Connect service enumeration (M2.20, requirement R5)

**Status:** enumeration note for the M2 networking milestone. Authoritative
specs: `specs/05-networking-p2p.md` §3.8 and `specs/15-serialization-and-wire-formats.md`
§3. This file records *which* gRPC/Connect services exist in `avalanchego`,
confirms that **inter-node P2P uses none of them**, and assigns each deferred
service to the later milestone that owns it. No service is implemented in M2.

## R5: inter-node P2P uses NO gRPC / Connect service

Inter-node peer-to-peer communication rides the **custom framed TLS stream**
(`specs/05` §1.1): a 4-byte big-endian length prefix followed by a proto3
`p2p.Message` (2 MiB cap). The `p2p` proto package (`proto/p2p/p2p.proto`,
`specs/15` §3.1) deliberately defines **no service** — it is just the message
schema carried over the raw TLS connection. There is no gRPC server, no Connect
endpoint, and no `tonic` transport anywhere on the node-to-node path.

The application-level request/response + gossip framework (`network/p2p/`,
`specs/05` §3.8 — the `ava-network::p2p` SDK in a later wave) is built **on top
of** `AppRequest`/`AppResponse`/`AppError`/`AppGossip`, which are themselves
`p2p.Message` variants. It therefore also rides the custom TLS wire protocol and
**is not gRPC**. Handler dispatch uses an in-band `uvarint(handler_id)` prefix on
the app bytes, not a gRPC method name.

> **gRPC/`tonic` is used only for the local out-of-process `rpcchainvm` plugin
> protocol** (host ⇄ plugin over a loopback socket — `specs/07`), and for the
> Connect-protocol HTTP services exposed by the API layer (`specs/12`). Neither
> is part of the inter-node networking surface that M2 owns.

## Deferred service inventory (owned by later milestones, NOT M2)

`avalanchego` ships **20 `.proto` files / 18 packages / 17 gRPC+Connect
services** (`specs/15` §3). Of these, the only one M2 (`ava-network`) consumes is
the **`p2p` message schema** (no service) for the framed TLS wire. Every actual
*service* below is out of scope for M2 and is listed here so the boundary is
explicit:

### gRPC services under `proto/` — owned by the rpcchainvm plugin milestone (`07`)

These form the `rpcchainvm` host ⇄ plugin protocol and its proxied callback
servers. Owner crate: `ava-vm-rpc` (+ `ava-vm`). Milestone: rpcchainvm
(post-M2).

| Service | Proto file | Role | Spec |
|---|---|---|---|
| `vm.VM` | `proto/vm/vm.proto` | Plugin VM protocol (v45) | `15` §3.2 |
| `vm.runtime.Runtime` | `proto/vm/runtime/runtime.proto` | Reverse-dial handshake | `15` §3.3 |
| `rpcdb.Database` | `proto/rpcdb/rpcdb.proto` | Database over gRPC | `15` §3.4 |
| `appsender.AppSender` | `proto/appsender/appsender.proto` | Outbound app messages from a plugin | `15` §3.5 |
| `sharedmemory.SharedMemory` | `proto/sharedmemory/sharedmemory.proto` | Atomic cross-chain memory | `15` §3.6 |
| `validatorstate.ValidatorState` | `proto/validatorstate/validator_state.proto` | Validator-set queries | `15` §3.7 |
| `warp.Signer` | `proto/warp/message.proto` | BLS warp-signature signer | `15` §3.8 |
| `signer.Signer` | `proto/signer/signer.proto` | BLS local-signer proxy | `15` §3.9 |
| `http.HTTP` | `proto/http/http.proto` | HTTP-over-gRPC for plugin API handlers | `15` §3.11 |
| `http.responsewriter.Writer` | `proto/http/responsewriter/responsewriter.proto` | Plugin → node ResponseWriter | `15` §3.12 |
| `io.reader.Reader` / `io.writer.Writer` / `net.conn.Conn` | `proto/io/...`, `proto/net/conn/...` | Hijack plumbing | `15` §3.13 |
| `aliasreader.AliasReader` | `proto/aliasreader/aliasreader.proto` | Chain-alias lookups | `15` §3.14 |

`proto/` packages with **no service** (helper message schemas, also not M2):
`sync` (merkledb state-sync proofs, `15` §3.10 — carried as app bytes),
`platformvm` (L1/ACP-77 warp justifications, `15` §3.15), `sdk` (the p2p
app-level SDK bodies, `15` §3.16). The `sdk` bodies are consumed by the
`ava-network::p2p` SDK in a later networking wave but are **not** a service.

### Connect services under `connectproto/` — owned by the API milestone (`12`)

These use the Connect protocol (HTTP), exposed by the node's API server. Owner
crate: `ava-api` (+ the respective VM). Milestone: API (post-M2).

| Service | Proto file | Notes | Spec |
|---|---|---|---|
| `proposervm.ProposerVM` | `connectproto/proposervm/service.proto` | `GetProposedHeight`, `GetCurrentEpoch` | `15` §3.17 |
| `xsvm.Ping` | `connectproto/xsvm/service.proto` | Only streaming service in the tree (bidi `StreamPing`) | `15` §3.18 |

## Summary

- **M2 / `ava-network` owns:** the framed TLS transport carrying the `p2p`
  message schema. No gRPC, no Connect.
- **Deferred to `07` (rpcchainvm, `ava-vm-rpc`):** every `proto/` gRPC service
  above.
- **Deferred to `12` (API, `ava-api`):** the two `connectproto/` Connect
  services.
