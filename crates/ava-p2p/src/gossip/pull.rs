// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `PullGossiper` — periodic bloom-filter pull requests (Go
//! `network/p2p/gossip/gossip.go` `PullGossiper[T]`).
//!
//! ## Simplifications recorded here (task-6 brief Step 1)
//!
//! - **Connected-peer pull sampling** (pre-authorized — see `gossip/mod.rs`'s
//!   [`super::GossipParams`] doc). Go wraps `PullGossiper` in a
//!   `ValidatorGossiper` (`gossip.go:97-103`, wired by
//!   `system.go:163-167`) so pull-gossip only ever runs while the local node
//!   is itself a validator, and its `p2p.Client` is constructed against a
//!   validator-aware peer set (`system.go:153`), so `AppRequestAny`'s
//!   `NodeSampler` samples *validators*. This port has no validator-stake
//!   sampler or `ValidatorGossiper` wrapper: [`PullGossiper::pull_cycle`]
//!   samples one peer via
//!   [`P2pNetwork::sample_peer`](crate::network::P2pNetwork::sample_peer),
//!   uniform over *all* connected peers; gating pull gossip to
//!   "only if I'm a validator" is left to a future caller if needed.
//! - **`pollSize` fixed at 1**, inlined rather than configurable. Go's
//!   `NewSystem` hardcodes `pollSize := 1` (`system.go:154`), i.e. exactly
//!   one `AppRequestAny` per `Gossip()` call — so
//!   [`PullGossiper::pull_cycle`] issuing exactly one `app_request` per call
//!   is the same constant, just without a dedicated config field.
//! - **No connected peer → silent no-op**, matching Go's
//!   `errors.Is(err, p2p.ErrNoPeers)` being swallowed rather than propagated
//!   (`gossip.go:250-255`).
//! - No metrics (`bloomFilterHitRate`/`observeMessage`, `gossip.go:280-317`).

use std::marker::PhantomData;
use std::sync::Arc;

use bytes::Bytes;
use prost::Message;
use tokio_util::sync::CancellationToken;

use ava_types::node_id::NodeId;
use ava_vm::app::AppError;

use crate::client::{Client, OnResponse};
use crate::error::Result;
use crate::gossip::{GossipParams, Gossipable, Marshaller, Set};
use crate::network::P2pNetwork;
use crate::pb::sdk;

/// Issues periodic bloom-filter pull requests to a sampled connected peer,
/// admitting whatever the peer sends back into the local [`Set`] (Go
/// `network/p2p/gossip/gossip.go` `PullGossiper[T]`).
pub struct PullGossiper<T, M, S> {
    marshaller: Arc<M>,
    set: Arc<S>,
    client: Client,
    network: Arc<P2pNetwork>,
    // `GossipParams` is stored for interface parity/future use (e.g. a
    // configurable `pollSize`); `pull_cycle` does not read it today (see the
    // module doc's `pollSize` note).
    _params: GossipParams,
    /// `T` never appears in a field directly (there is no queue of `T` to
    /// drain, unlike `PushGossiper`) — it only shows up in the `M`/`S` trait
    /// bounds — so an explicit marker is required to keep `T` a used type
    /// parameter. `fn() -> T` (rather than `T` itself) keeps this struct's
    /// auto-trait (`Send`/`Sync`) derivation from depending on `T`'s own.
    _marker: PhantomData<fn() -> T>,
}

impl<T, M, S> PullGossiper<T, M, S>
where
    T: Gossipable + 'static,
    M: Marshaller<T> + 'static,
    S: Set<T> + 'static,
{
    /// Constructs a `PullGossiper` (Go `NewPullGossiper`, `gossip.go:204-220`).
    #[must_use]
    pub fn new(
        marshaller: M,
        set: Arc<S>,
        client: Client,
        network: Arc<P2pNetwork>,
        params: GossipParams,
    ) -> Self {
        Self {
            marshaller: Arc::new(marshaller),
            set,
            client,
            network,
            _params: params,
            _marker: PhantomData,
        }
    }

    /// Runs one pull cycle (Go `PullGossiper.Gossip`, `gossip.go:243-258`):
    /// samples one connected peer, sends it a `PullGossipRequest` built from
    /// `set.get_filter()`, and registers a callback
    /// ([`Self::handle_response`]) that admits every item the peer returns
    /// into the set. A `None` sample (no connected peers) is a silent no-op.
    pub async fn pull_cycle(&self, token: &CancellationToken) -> Result<()> {
        let Some(peer) = self.network.sample_peer() else {
            return Ok(());
        };

        let (filter, salt) = self.set.get_filter();
        let request = sdk::PullGossipRequest {
            salt: Bytes::from(salt),
            filter: Bytes::from(filter),
        };
        let bytes = request.encode_to_vec();

        let marshaller = self.marshaller.clone();
        let set = self.set.clone();
        let on_response: OnResponse = Box::new(move |node, result| {
            Self::handle_response(&marshaller, &set, node, result);
        });

        self.client
            .app_request(token, peer, bytes, on_response)
            .await
    }

    /// Decodes a `PullGossipResponse` and admits every item into `set`,
    /// logging and skipping malformed entries rather than failing the whole
    /// response (Go `PullGossiper.handleResponse`, `gossip.go:260-317`).
    fn handle_response(
        marshaller: &M,
        set: &S,
        node: NodeId,
        result: std::result::Result<Vec<u8>, AppError>,
    ) {
        let response_bytes = match result {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::debug!(%node, error = %err, "failed gossip request");
                return;
            }
        };
        let response = match sdk::PullGossipResponse::decode(response_bytes.as_slice()) {
            Ok(response) => response,
            Err(err) => {
                tracing::debug!(%node, error = %err, "failed to unmarshal gossip response");
                return;
            }
        };
        for item_bytes in response.gossip {
            let item = match marshaller.unmarshal(&item_bytes) {
                Ok(item) => item,
                Err(err) => {
                    tracing::debug!(%node, error = %err, "failed to unmarshal gossip");
                    continue;
                }
            };
            let id = item.gossip_id();
            if let Err(err) = set.add(item) {
                tracing::debug!(%node, %id, error = %err, "failed to add gossip to known set");
            }
        }
    }
}
