// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-chain wallet — atomic import/export builder / signer / backend (port of
//! `wallet/chain/c`). Implemented in M8.26.

// Consumed from M8.26 (C-chain atomic tx types); referenced to satisfy
// unused-crate-dependencies until the builder lands in the next task commit.
use ava_evm as _;
