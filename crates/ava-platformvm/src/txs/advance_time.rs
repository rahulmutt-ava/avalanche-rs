// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.AdvanceTimeTx` (type_id 19) — the Apricot-only proposal tx that advances
//! chain time (specs 08 §2.2). No embedded `BaseTx`.

use ava_codec::AvaCodec;

/// `txs.AdvanceTimeTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct AdvanceTimeTx {
    /// Unix time this block proposes increasing the timestamp to.
    #[codec]
    pub time: u64,
}
