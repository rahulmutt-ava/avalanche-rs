// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `get_node_config` — the order-sensitive resolution of the layered flags
//! into the node [`Config`] (Go `config/config.go::GetNodeConfig`,
//! specs 12 §1.6, 13 §3/§5/§7/§8/§13/§18/§19/§21).

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;
    use crate::ConfigError;
    use crate::flags::{FLAG_SPECS, build_command};
    use crate::precedence::Layered;
    use crate::subnets::PRIMARY_NETWORK_ID;

    /// Builds a `Layered` over a fresh tempdir data dir (so the plugin-dir /
    /// staking-cert side effects stay sandboxed) with an ephemeral staking
    /// cert (no disk keygen) and runs `get_node_config`.
    fn node_config(
        args: &[&str],
    ) -> (crate::Result<crate::node::Config>, tempfile::TempDir) {
        let data = tempfile::tempdir().expect("tempdir");
        let mut all = vec![
            "avalanchers".to_string(),
            format!("--data-dir={}", data.path().display()),
            "--staking-ephemeral-cert-enabled=true".to_string(),
        ];
        all.extend(args.iter().map(ToString::to_string));
        let layered =
            Layered::build_with_env(build_command(FLAG_SPECS), all, FLAG_SPECS, std::iter::empty())
                .expect("layered");
        (get_node_config(&layered), data)
    }

    #[test]
    fn network_allow_private_ips_dependence() {
        // Unset: false for Mainnet/Fuji (production networks), true otherwise
        // (Go getNetworkConfig: !ProductionNetworkIDs.Contains; 13 §8).
        // Set: honored verbatim.
        let cases: [(&str, Option<bool>, bool); 6] = [
            ("mainnet", None, false),
            ("fuji", None, false),
            ("local", None, true),
            ("1337", None, true),
            ("mainnet", Some(true), true),
            ("local", Some(false), false),
        ];
        for (network, set, want) in cases {
            let mut args = vec![format!("--network-id={network}")];
            if let Some(v) = set {
                args.push(format!("--network-allow-private-ips={v}"));
            }
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let (config, _dir) = node_config(&args);
            let config = config.unwrap_or_else(|e| panic!("{network}/{set:?}: {e}"));
            assert_eq!(
                config.network_config.allow_private_ips, want,
                "{network}/{set:?}"
            );
        }
    }

    #[test]
    fn sybil_protection_disabled_rejected_on_mainnet() {
        // Go errSybilProtectionDisabledOnPublicNetwork (13 §5).
        for network in ["mainnet", "fuji"] {
            let (config, _dir) = node_config(&[
                &format!("--network-id={network}"),
                "--sybil-protection-enabled=false",
            ]);
            assert_matches!(
                config,
                Err(ConfigError::SybilProtectionDisabledOnPublicNetwork),
                "{network}"
            );
        }

        // Allowed on local; recorded in the staking config.
        let (config, _dir) =
            node_config(&["--network-id=local", "--sybil-protection-enabled=false"]);
        let config = config.expect("local");
        assert!(!config.staking_config.sybil_protection_enabled);

        // Disabled weight must be positive when sybil protection is off.
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--sybil-protection-enabled=false",
            "--sybil-protection-disabled-weight=0",
        ]);
        assert_matches!(
            config,
            Err(ConfigError::SybilProtectionDisabledStakerWeights)
        );
    }

    #[test]
    fn bootstrappers_filled_from_genesis_when_unset() {
        // Both unset + standard network => genesis.SampleBootstrappers(net, 5)
        // (13 §13).
        let (config, _dir) = node_config(&["--network-id=fuji"]);
        let config = config.expect("fuji");
        let beacons = ava_genesis::bootstrappers(ava_types::constants::FUJI_ID);
        assert_eq!(config.bootstrap_config.bootstrappers.len(), 5.min(beacons.len()));
        for b in &config.bootstrap_config.bootstrappers {
            assert!(beacons.contains(b), "sampled bootstrapper not in genesis list");
        }

        // Both unset + custom network => empty.
        let (config, _dir) = node_config(&["--network-id=1337"]);
        assert!(config.expect("custom").bootstrap_config.bootstrappers.is_empty());

        // Mutually required: one set without the other errors.
        let (config, _dir) = node_config(&["--network-id=local", "--bootstrap-ips=127.0.0.1:9651"]);
        assert_matches!(config, Err(ConfigError::BootstrapMutuallyRequired { .. }));
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--bootstrap-ids=NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg",
        ]);
        assert_matches!(config, Err(ConfigError::BootstrapMutuallyRequired { .. }));

        // Mismatched counts error.
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--bootstrap-ips=127.0.0.1:9651,127.0.0.2:9651",
            "--bootstrap-ids=NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg",
        ]);
        assert_matches!(config, Err(ConfigError::BootstrapPeerCountMismatch { ips: 2, ids: 1 }));

        // Matching counts are zipped.
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--bootstrap-ips=127.0.0.1:9651",
            "--bootstrap-ids=NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg",
        ]);
        let config = config.expect("local");
        assert_eq!(config.bootstrap_config.bootstrappers.len(), 1);
        assert_eq!(
            config.bootstrap_config.bootstrappers[0].ip,
            "127.0.0.1:9651".parse().expect("addr")
        );
    }

    #[test]
    fn snow_quorum_overrides_alpha() {
        // --snow-quorum-size overrides BOTH alphaPreference and
        // alphaConfidence; the dedicated flags are ignored (13 §7).
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--snow-quorum-size=18",
            "--snow-preference-quorum-size=14",
            "--snow-confidence-quorum-size=16",
        ]);
        let config = config.expect("local");
        let primary = config
            .subnet_configs
            .get(&PRIMARY_NETWORK_ID)
            .expect("primary network subnet config");
        let snow = primary.snow_parameters.as_ref().expect("snow params");
        assert_eq!(snow.alpha_preference, 18);
        assert_eq!(snow.alpha_confidence, 18);

        // Without it, the dedicated flags are honored.
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--snow-preference-quorum-size=14",
        ]);
        let config = config.expect("local");
        let snow = config
            .subnet_configs
            .get(&PRIMARY_NETWORK_ID)
            .expect("primary")
            .snow_parameters
            .clone()
            .expect("snow params");
        assert_eq!(snow.alpha_preference, 14);
        assert_eq!(snow.alpha_confidence, 15); // default

        // The benchlist MaxPortion derives from the primary alpha/k.
        let max_portion = config.benchlist_config.max_portion;
        assert!((max_portion - (1.0 - 15.0 / 20.0) / 3.0).abs() < 1e-12);
    }

    #[test]
    fn staking_economics_and_fees_ignored_on_standard_networks() {
        // 13 §4/§5: the fee + staking-economics flags only apply to
        // non-standard networks; Mainnet/Fuji use the genesis params.
        let (config, _dir) = node_config(&[
            "--network-id=fuji",
            "--tx-fee=123",
            "--uptime-requirement=0.5",
        ]);
        let config = config.expect("fuji");
        assert_eq!(config.tx_fee_config.tx_fee, 1_000_000);
        assert!((config.staking_config.economics.uptime_requirement - 0.8).abs() < 1e-12);

        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--tx-fee=123",
            "--uptime-requirement=0.5",
        ]);
        let config = config.expect("local");
        assert_eq!(config.tx_fee_config.tx_fee, 123);
        assert!((config.staking_config.economics.uptime_requirement - 0.5).abs() < 1e-12);

        // Genesis data resolved alongside (embedded for standard networks).
        assert!(!config.genesis_bytes.is_empty());
        assert_ne!(config.avax_asset_id, ava_types::id::Id::EMPTY);
    }

    #[test]
    fn one_of_validations() {
        // Staking signer: at most one option (Go errInvalidSignerConfig).
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--staking-ephemeral-signer-enabled=true",
            "--staking-rpc-signer-endpoint=http://signer",
        ]);
        assert_matches!(config, Err(ConfigError::InvalidSignerConfig));

        // public-ip XOR public-ip-resolution-service (13 §19).
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--public-ip=1.2.3.4",
            "--public-ip-resolution-service=opendns",
        ]);
        assert_matches!(config, Err(ConfigError::ConflictingPublicIpOptions));

        // Disk space percentages: warn <= 50, warn >= required (13 §18).
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--system-tracker-disk-warning-available-space-percentage=60",
        ]);
        assert_matches!(config, Err(ConfigError::DiskSpaceOutOfRange { .. }));
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--system-tracker-disk-warning-available-space-percentage=5",
            "--system-tracker-disk-required-available-space-percentage=10",
        ]);
        assert_matches!(config, Err(ConfigError::DiskWarnAfterFatal { .. }));

        // track-subnets must not contain the Primary Network (13 §14).
        let (config, _dir) = node_config(&[
            "--network-id=local",
            &format!("--track-subnets={PRIMARY_NETWORK_ID}"),
        ]);
        assert_matches!(config, Err(ConfigError::CannotTrackPrimaryNetwork));
    }
}
