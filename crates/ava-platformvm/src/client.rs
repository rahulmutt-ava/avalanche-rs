// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain JSON-RPC **read** client — typed async wrappers over the read
//! methods of [`service`](crate::service) (port of the read-relevant parts of
//! `vms/platformvm/client.go`, specs 08 §9, 14).
//!
//! The client is structured around a [`Transport`] seam: a typed wrapper builds
//! the method name + JSON params, hands them to the transport, and deserializes
//! the JSON reply into the corresponding `service` reply type. The concrete HTTP
//! transport (reqwest over the node's `/ext/bc/P` endpoint) is **deferred** to
//! `ava-api` (M8/M12); this module provides the typed surface and a test
//! transport so the request/response serialization round-trips are exercised.

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;

use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::error::{Error, Result};
use crate::service::{
    GetCurrentSupplyArgs, GetCurrentSupplyReply, GetCurrentValidatorsArgs,
    GetCurrentValidatorsReply, GetHeightResponse, GetL1ValidatorArgs, GetL1ValidatorReply,
    GetTimestampReply, GetValidatorsAtReply,
};

/// The JSON-RPC transport seam: send `method` with `params` and decode the
/// reply. The HTTP implementation is deferred (M8/M12).
#[async_trait]
pub trait Transport: Send + Sync {
    /// Invokes the JSON-RPC `method` with serialized `params`, returning the
    /// JSON `result` value.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on transport / decode failure.
    async fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value>;
}

/// A typed P-Chain read client over a [`Transport`].
pub struct Client<T: Transport> {
    transport: T,
}

impl<T: Transport> Client<T> {
    /// Builds a client over `transport`.
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Serializes `params`, dispatches `method`, and decodes the reply.
    async fn request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R> {
        let params = serde_json::to_value(params)
            .map_err(|e| Error::Service(format!("encode params for {method}: {e}")))?;
        let value = self.transport.call(method, params).await?;
        serde_json::from_value(value)
            .map_err(|e| Error::Service(format!("decode reply for {method}: {e}")))
    }

    /// `platform.getHeight`.
    pub async fn get_height(&self) -> Result<GetHeightResponse> {
        self.request("platform.getHeight", &serde_json::json!({}))
            .await
    }

    /// `platform.getTimestamp`.
    pub async fn get_timestamp(&self) -> Result<GetTimestampReply> {
        self.request("platform.getTimestamp", &serde_json::json!({}))
            .await
    }

    /// `platform.getCurrentSupply`.
    pub async fn get_current_supply(&self, subnet_id: Id) -> Result<GetCurrentSupplyReply> {
        self.request(
            "platform.getCurrentSupply",
            &GetCurrentSupplyArgs { subnet_id },
        )
        .await
    }

    /// `platform.getCurrentValidators`.
    pub async fn get_current_validators(
        &self,
        subnet_id: Id,
        node_ids: Vec<NodeId>,
    ) -> Result<GetCurrentValidatorsReply> {
        self.request(
            "platform.getCurrentValidators",
            &GetCurrentValidatorsArgs {
                subnet_id,
                node_ids,
            },
        )
        .await
    }

    /// `platform.getL1Validator`.
    pub async fn get_l1_validator(&self, validation_id: Id) -> Result<GetL1ValidatorReply> {
        self.request(
            "platform.getL1Validator",
            &GetL1ValidatorArgs { validation_id },
        )
        .await
    }

    /// `platform.getValidatorsAt`.
    pub async fn get_validators_at(
        &self,
        height: u64,
        subnet_id: Id,
    ) -> Result<GetValidatorsAtReply> {
        self.request(
            "platform.getValidatorsAt",
            &serde_json::json!({ "height": height, "subnetID": subnet_id }),
        )
        .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::service::{ApiValidator, GetCurrentValidatorsReply};

    /// A transport that echoes a recorded JSON reply, asserting the dispatched
    /// method name. Exercises the client's params-encode / reply-decode path.
    struct StubTransport {
        expect_method: &'static str,
        reply: serde_json::Value,
    }

    #[async_trait]
    impl Transport for StubTransport {
        async fn call(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<serde_json::Value> {
            assert_eq!(method, self.expect_method);
            Ok(self.reply.clone())
        }
    }

    #[tokio::test]
    async fn client_get_height_decodes() {
        let client = Client::new(StubTransport {
            expect_method: "platform.getHeight",
            reply: serde_json::json!({ "height": "42" }),
        });
        let reply = client.get_height().await.expect("height");
        assert_eq!(reply.height, 42);
    }

    #[tokio::test]
    async fn client_get_current_validators_roundtrip() {
        // Build a reply via the service types, serialize it, and confirm the
        // client decodes it back identically (the request/response contract).
        let original = GetCurrentValidatorsReply {
            validators: vec![ApiValidator {
                tx_id: Id::from([0x01; 32]),
                node_id: NodeId::from([0x0A; 20]),
                weight: 1_000_000,
                start_time: 1_600_000_000,
                public_key: Some("0xdeadbeef".to_string()),
                validation_id: None,
                min_nonce: None,
            }],
        };
        let client = Client::new(StubTransport {
            expect_method: "platform.getCurrentValidators",
            reply: serde_json::to_value(&original).unwrap(),
        });
        let decoded = client
            .get_current_validators(Id::EMPTY, vec![])
            .await
            .expect("validators");
        assert_eq!(decoded, original);
    }
}
