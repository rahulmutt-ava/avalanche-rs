// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `GossipHandler` — answers pull requests and admits pushed gossip (Go
//! `network/p2p/gossip/handler.go` `Handler[T]`).
//!
//! ## Simplifications recorded here (task-6 brief Step 1)
//!
//! - **Direct push-forwarding wiring.** Go's `Handler.AppGossip` only calls
//!   `h.set.Add(gossipable)` (`handler.go:115-149`) — it never touches a
//!   `PushGossiper` directly. Transitive "forward what I was just told
//!   about" happens one layer down, in the concrete `Set` (the tx pool):
//!   `ethTxPool.Subscribe` feeds newly-admitted txs to
//!   `vm.ethTxPushGossiper.Add` (`graft/coreth/plugin/evm/vm.go:778-824`) —
//!   i.e. *any* `Set.Add` (from a push **or** a pull) ends up re-queued for
//!   push, via the pool's own event subscription, not via the gossip
//!   `Handler`. This port has no tx-pool/subscription layer yet (a later
//!   task), so — per the task-6 brief — [`GossipHandler`] takes an
//!   `Option<Arc<PushGossiper<T, M, S>>>` and calls `push.add(item)`
//!   directly after a successful `set.add(item)` inside `app_gossip`,
//!   folding what Go does via the tx-pool subscription into the handler
//!   itself. (Note this only wires the *push*-received side; the
//!   pull-received side, `PullGossiper::handle_response`, does not forward
//!   to a `PushGossiper` either here or in Go — Go's forwarding is genuinely
//!   driven by the pool's subscription, which fires for both paths equally;
//!   this port's narrower per-path wiring is *also* part of this
//!   simplification.)
//! - **Handler is a bare struct, not `p2p.NoOpHandler`-wrapped.** Go's
//!   `Handler[T]` embeds `p2p.Handler` (`p2p.NoOpHandler{}`) to pick up
//!   default no-op `Connected`/`Disconnected`/`CrossChainAppRequest` methods
//!   (`handler.go:33-48`); this port's [`crate::handler::Handler`] trait only
//!   has the two methods `GossipHandler` needs, so there is nothing to
//!   default.
//! - **Marshal failure aborts the whole request**, matching Go: a
//!   marshal error while building the response sets `Iterate`'s captured
//!   `err` and stops iteration (`handler.go:81-96`), and the outer
//!   `AppRequest` then discards any partial batch and returns
//!   `ErrUnexpected` rather than sending what it had so far
//!   (`handler.go:94-96`).
//! - Malformed request bytes and an unparsable bloom filter both map to a
//!   single `err_unexpected()`, matching Go's `ParseAppRequest` failure
//!   branch (`handler.go:59-63`), which likewise doesn't distinguish "bad
//!   proto" from "bad bloom filter".
//! - No metrics (`bloomFilterHitRate`/`observeMessage`, `handler.go:64-113`).

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use bytes::Bytes;
use prost::Message;

use ava_types::node_id::NodeId;
use ava_utils::bloom::ReadFilter;
use ava_vm::app::AppError;

use crate::gossip::push::PushGossiper;
use crate::gossip::{GossipParams, Gossipable, Marshaller, Set};
use crate::handler::{Handler, err_unexpected};
use crate::pb::sdk;

/// Answers pull requests and admits pushed gossip into a [`Set`], optionally
/// forwarding newly-admitted pushed items to a [`PushGossiper`] (Go
/// `network/p2p/gossip/handler.go` `Handler[T]`).
pub struct GossipHandler<T, M, S> {
    marshaller: Arc<M>,
    set: Arc<S>,
    push: Option<Arc<PushGossiper<T, M, S>>>,
    params: GossipParams,
}

impl<T, M, S> GossipHandler<T, M, S>
where
    T: Gossipable,
    M: Marshaller<T>,
    S: Set<T>,
{
    /// Constructs a `GossipHandler` (Go `NewHandler`, `handler.go:33-48`).
    #[must_use]
    pub fn new(
        marshaller: M,
        set: Arc<S>,
        push: Option<Arc<PushGossiper<T, M, S>>>,
        params: GossipParams,
    ) -> Self {
        Self {
            marshaller: Arc::new(marshaller),
            set,
            push,
            params,
        }
    }
}

#[async_trait]
impl<T, M, S> Handler for GossipHandler<T, M, S>
where
    T: Gossipable + 'static,
    M: Marshaller<T> + 'static,
    S: Set<T> + 'static,
{
    /// Admits a `PushGossip` batch into the set, forwarding each
    /// successfully-admitted item to `push` if configured (Go
    /// `Handler.AppGossip`, `handler.go:115-149`, plus this port's
    /// forwarding wiring — see the module doc).
    async fn app_gossip(&self, node: NodeId, msg: &[u8]) {
        let push_gossip = match sdk::PushGossip::decode(msg) {
            Ok(msg) => msg,
            Err(err) => {
                tracing::debug!(%node, error = %err, "failed to unmarshal gossip");
                return;
            }
        };

        for item_bytes in push_gossip.gossip {
            let item = match self.marshaller.unmarshal(&item_bytes) {
                Ok(item) => item,
                Err(err) => {
                    tracing::debug!(%node, error = %err, "failed to unmarshal gossip");
                    continue;
                }
            };
            let id = item.gossip_id();
            if let Err(err) = self.set.add(item) {
                tracing::debug!(%node, %id, error = %err, "failed to add gossip to known set");
                continue;
            }

            let Some(push) = &self.push else { continue };
            // The set already consumed the first decode above; unmarshal
            // the same bytes again to hand `push.add` its own owned `T`
            // (no `Clone` bound on `T`/`Marshaller` is required this way).
            match self.marshaller.unmarshal(&item_bytes) {
                Ok(forwarded) => push.add(forwarded),
                Err(_) => {
                    // Unreachable in practice: the same bytes just decoded
                    // successfully above.
                }
            }
        }
    }

    /// Answers a `PullGossipRequest` with items the requester's filter says
    /// it doesn't have, stopping once `target_message_size` is reached (Go
    /// `Handler.AppRequest`, `handler.go:59-113`).
    async fn app_request(
        &self,
        _node: NodeId,
        _deadline: Instant,
        msg: &[u8],
    ) -> std::result::Result<Vec<u8>, AppError> {
        let request = sdk::PullGossipRequest::decode(msg).map_err(|_| err_unexpected())?;
        let filter = ReadFilter::parse(&request.filter).map_err(|_| err_unexpected())?;
        let salt = request.salt;

        let mut gossip_bytes: Vec<Bytes> = Vec::new();
        let mut response_size = 0usize;
        let mut marshal_failed = false;
        let target = self.params.target_message_size;
        self.set.iterate(&mut |item: &T| {
            let id = item.gossip_id();
            // Filter out what the requesting peer already knows about.
            if filter.contains_key(id.as_bytes(), &salt) {
                return true;
            }

            match self.marshaller.marshal(item) {
                Ok(bytes) => {
                    response_size = response_size.saturating_add(bytes.len());
                    gossip_bytes.push(Bytes::from(bytes));
                    response_size <= target
                }
                Err(_) => {
                    marshal_failed = true;
                    false
                }
            }
        });
        if marshal_failed {
            return Err(err_unexpected());
        }

        let response = sdk::PullGossipResponse {
            gossip: gossip_bytes,
        };
        Ok(response.encode_to_vec())
    }
}
