// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init step 8 (specs/12 §2.2): NAT router probe + port mapper (mirror Go
//! `initNAT`).

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use ava_config::node::IpConfig;

use crate::error::Result;
use crate::nat::{Mapper, NatRouter, NoRouter};

/// An object-safe [`NatRouter`] holder: [`Mapper`] (`PortMapper<R>`) is generic
/// over a sized router, while [`crate::nat::get_router`] returns a
/// `Box<dyn NatRouter>`; this delegating newtype bridges the two.
pub struct BoxedNatRouter(Box<dyn NatRouter>);

impl NatRouter for BoxedNatRouter {
    fn supports_nat(&self) -> bool {
        self.0.supports_nat()
    }

    fn map_port(
        &self,
        internal: u16,
        external: u16,
        desc: &str,
        duration: Duration,
    ) -> ava_network::Result<()> {
        self.0.map_port(internal, external, desc, duration)
    }

    fn unmap_port(&self, internal: u16, external: u16) -> ava_network::Result<()> {
        self.0.unmap_port(internal, external)
    }

    fn external_ip(&self) -> ava_network::Result<IpAddr> {
        self.0.external_ip()
    }
}

/// The node's NAT handles (Go `n.router` + `n.portMapper`).
pub struct Nat {
    /// The probed (or no-op) NAT router.
    pub router: Arc<BoxedNatRouter>,
    /// The port mapper spawning keep-alive tasks per mapped port.
    pub mapper: Mapper<BoxedNatRouter>,
}

/// Step 8: probe for a NAT router (UPnP → NAT-PMP → no-op) unless a public IP
/// or resolution service is configured, then build the port mapper (mirror Go
/// `initNAT`).
///
/// The router probe performs blocking network I/O (`igd-next` + the RFC 6886
/// UDP exchange), so it runs on the blocking pool (M8.28 handoff; 17 §1.2).
///
/// # Errors
/// [`crate::error::Error::Join`] if the blocking probe task is cancelled.
pub async fn init_nat(ip_config: &IpConfig) -> Result<Nat> {
    tracing::info!("initializing NAT");

    let router: Box<dyn NatRouter> =
        if ip_config.public_ip.is_empty() && ip_config.public_ip_resolution_service.is_empty() {
            let probed = tokio::task::spawn_blocking(crate::nat::get_router).await?;
            if !probed.supports_nat() {
                tracing::warn!(
                    "UPnP and NAT-PMP router attach failed, you may not be listening publicly. \
                     Please confirm the settings in your router"
                );
            }
            probed
        } else {
            Box::new(NoRouter::new())
        };

    let router = Arc::new(BoxedNatRouter(router));
    let mapper = Mapper::new(Arc::clone(&router));
    Ok(Nat { router, mapper })
}
