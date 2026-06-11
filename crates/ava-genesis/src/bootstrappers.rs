// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `bootstrappers.go` — the embedded per-network beacon lists and the uniform
//! bootstrapper sampling (specs 23 §5.2).
//!
//! `bootstrappers.json` is embedded verbatim and parsed once into a
//! `network name → Vec<Bootstrapper>` map. Custom networks have no embedded
//! beacons (they are fed from `--bootstrap-ips/-ids`, specs 12 §1.6).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use ava_types::constants;
use ava_types::node_id::NodeId;
use ava_utils::rng::{Mt19937_64, Source};
use ava_utils::sampler::uniform::{Uniform, UniformReplacer};

/// `genesis/bootstrappers.json`, embedded verbatim.
pub static BOOTSTRAPPERS_JSON: &str = include_str!("../data/bootstrappers.json");

/// `genesis.Bootstrapper` — the relationship between a node id and its ip
/// (sometimes called an "anchor" or "beacon" node).
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Bootstrapper {
    /// `id` — the beacon's node id (`NodeID-<cb58>`).
    pub id: NodeId,
    /// `ip` — the beacon's `<ip>:<port>` socket address.
    pub ip: SocketAddr,
}

/// The parsed `network name → beacons` map. Panics on malformed embedded JSON
/// exactly like Go's `bootstrappers.go::init()` (a compile-time-constant input).
static BOOTSTRAPPERS_PER_NETWORK: LazyLock<HashMap<String, Vec<Bootstrapper>>> =
    LazyLock::new(|| {
        serde_json::from_str(BOOTSTRAPPERS_JSON)
            .unwrap_or_else(|e| panic!("failed to decode bootstrappers.json {e}"))
    });

/// `GetBootstrappers` — all default bootstrappers for `network_id` (empty for
/// custom networks).
#[must_use]
pub fn bootstrappers(network_id: u32) -> Vec<Bootstrapper> {
    BOOTSTRAPPERS_PER_NETWORK
        .get(&constants::network_name(network_id))
        .cloned()
        .unwrap_or_default()
}

/// `SampleBootstrappers` — `min(count, len)` distinct beacons drawn with the
/// uniform sampler, seeded from the wall clock (Go's sampler RNG is similarly
/// non-cryptographic and run-dependent; never a consensus input).
#[must_use]
pub fn sample_bootstrappers(network_id: u32, count: usize) -> Vec<Bootstrapper> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let mut src = Mt19937_64::new();
    src.seed(nanos);
    sample_bootstrappers_with(network_id, count, Box::new(src))
}

/// [`sample_bootstrappers`] with an injected RNG [`Source`] — the deterministic
/// form (the selection sequence is fixed by the source; gonum-parity sampler,
/// specs 03 §4.1).
#[must_use]
pub fn sample_bootstrappers_with(
    network_id: u32,
    count: usize,
    src: Box<dyn Source>,
) -> Vec<Bootstrapper> {
    let all = bootstrappers(network_id);
    let count = count.min(all.len());

    let mut sampler = UniformReplacer::new(src);
    sampler.initialize(all.len() as u64);
    let Some(indices) = sampler.sample(count) else {
        return Vec::new();
    };

    indices
        .iter()
        .filter_map(|&index| all.get(usize::try_from(index).unwrap_or(usize::MAX)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ava_types::constants::{FUJI_ID, LOCAL_ID, MAINNET_ID};

    use super::*;

    /// M8.14 red test: per-network count + IDs + IPs match
    /// `bootstrappers.json` (23 §9.5), and the sampler is deterministic for a
    /// fixed source + draws distinct in-set entries (Go
    /// `TestSampleBootstrappers`).
    #[test]
    fn bootstrapper_parity() {
        // Counts + first/last entries, verbatim from bootstrappers.json.
        let mainnet = bootstrappers(MAINNET_ID);
        assert_eq!(mainnet.len(), 24);
        assert_eq!(
            mainnet.first().expect("mainnet[0]").id.to_string(),
            "NodeID-A6onFGyJjA37EZ7kYHANMR1PFRT8NmXrF"
        );
        assert_eq!(
            mainnet.first().expect("mainnet[0]").ip,
            "54.232.137.108:9651".parse::<SocketAddr>().expect("ip")
        );
        assert_eq!(
            mainnet.last().expect("mainnet[-1]").id.to_string(),
            "NodeID-FYv1Lb29SqMpywYXH7yNkcFAzRF2jvm3K"
        );

        let fuji = bootstrappers(FUJI_ID);
        assert_eq!(fuji.len(), 21);
        assert_eq!(
            fuji.first().expect("fuji[0]").id.to_string(),
            "NodeID-2m38qc95mhHXtrhjyGbe7r2NhniqHHJRB"
        );
        assert_eq!(
            fuji.last().expect("fuji[-1]").ip,
            "54.20.25.221:9651".parse::<SocketAddr>().expect("ip")
        );

        // Local + custom networks have no embedded beacons.
        assert!(bootstrappers(LOCAL_ID).is_empty());
        assert!(bootstrappers(9999).is_empty());

        // Go TestSampleBootstrappers: mainnet/fuji yield exactly `length`.
        for net in [MAINNET_ID, FUJI_ID] {
            let sampled = sample_bootstrappers(net, 10);
            assert_eq!(sampled.len(), 10);
            let all: HashSet<NodeId> = bootstrappers(net).iter().map(|b| b.id).collect();
            let distinct: HashSet<NodeId> = sampled.iter().map(|b| b.id).collect();
            assert_eq!(distinct.len(), 10, "sampled beacons must be distinct");
            assert!(distinct.is_subset(&all), "sampled beacons must be in-set");
        }
        // Oversampling clamps to the list length.
        assert_eq!(sample_bootstrappers(MAINNET_ID, 1000).len(), 24);
        assert!(sample_bootstrappers(LOCAL_ID, 10).is_empty());
    }

    /// The injected-source form is deterministic: same seed ⇒ same selection
    /// sequence (the gonum-parity MT19937-64 + uniform-replacer draw path,
    /// specs 03 §4.1).
    #[test]
    fn sample_bootstrappers_deterministic() {
        let sample = |seed: u64| {
            let mut src = Mt19937_64::new();
            src.seed(seed);
            sample_bootstrappers_with(MAINNET_ID, 5, Box::new(src))
        };
        let a = sample(42);
        let b = sample(42);
        assert_eq!(a.len(), 5);
        assert_eq!(a, b, "fixed seed must reproduce the selection");
        assert_eq!(sample(43).len(), 5);
    }
}
