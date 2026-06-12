// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init step 13 (specs/12 §2.2): the `message::Creator` — built **after**
//! metrics and **before** networking / chain manager / engine (the Go comment
//! is load-bearing: the creator records under the `network` namespace).

use std::sync::Arc;

use ava_config::node::NetworkConfig;
use ava_message::builder::Creator;
use ava_message::codec::{Compression, MsgBuilder};

use crate::error::{Error, Result};
use crate::init::metrics::NodeMetrics;

/// Build the message creator + the shared `avalanche_network` registry the
/// networking layer also registers against (mirror Go `message.NewCreator`
/// over `networkRegisterer`).
///
/// # Errors
/// - Metrics-namespace registration failures.
/// - [`Error::UnknownCompressionType`] for an unrecognized
///   `--network-compression-type`.
pub fn init_message_creator(
    network_config: &NetworkConfig,
    metrics: &NodeMetrics,
) -> Result<(Arc<Creator>, prometheus::Registry)> {
    let network_registry = ava_api::metrics::make_and_register(
        metrics.gatherer.as_ref(),
        &crate::init::namespace::network(),
    )?;

    let compression = match network_config.compression_type.as_str() {
        "zstd" => Compression::Zstd,
        "none" => Compression::None,
        other => return Err(Error::UnknownCompressionType(other.to_owned())),
    };

    let builder = MsgBuilder::new(network_config.maximum_inbound_message_timeout);
    let creator = Arc::new(Creator::with_compression(Arc::new(builder), compression));
    Ok((creator, network_registry))
}
