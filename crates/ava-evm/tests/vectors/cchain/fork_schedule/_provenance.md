# C-Chain fork-schedule golden vector — provenance

`mainnet.json` is the recorded oracle for `chainspec::tests::fork_at_and_spec_id_match_coreth`
(M6.5, spec 10 §7.4/§17.8, G7). It pins, for each Avalanche network-upgrade
phase on **Avalanche mainnet**: the phase name, its activation **unix second**,
and the revm Ethereum `SpecId` `AvaChainSpec::revm_spec_id` must return at that
phase.

## Activation timestamps

Source of truth: `ava_version::upgrade::mainnet_config()` (crate `ava-version`),
which mirrors avalanchego `upgrade/upgrade.go` mainnet constants verbatim. The
`u64` unix seconds here are `DateTime<Utc>::timestamp()` of those constants:

| Phase             | UTC (upgrade.go)        | unix second  |
|-------------------|-------------------------|--------------|
| Launch            | (genesis / pre-AP1)     | 0            |
| ApricotPhase1     | 2021-03-31 14:00:00     | 1617199200   |
| ApricotPhase2     | 2021-05-10 11:00:00     | 1620644400   |
| ApricotPhase3     | 2021-08-24 14:00:00     | 1629813600   |
| ApricotPhase4     | 2021-09-22 21:00:00     | 1632344400   |
| ApricotPhase5     | 2021-12-02 18:00:00     | 1638468000   |
| ApricotPhasePre6  | 2022-09-05 01:30:00     | 1662341400   |
| ApricotPhase6     | 2022-09-06 20:00:00     | 1662494400   |
| ApricotPhasePost6 | 2022-09-07 03:00:00     | 1662519600   |
| Banff             | 2022-10-18 16:00:00     | 1666108800   |
| Cortina           | 2023-04-25 15:00:00     | 1682434800   |
| Durango           | 2024-03-06 16:00:00     | 1709740800   |
| Etna              | 2024-12-16 17:00:00     | 1734368400   |
| Fortuna           | 2025-04-08 15:00:00     | 1744124400   |
| Granite           | 2025-11-19 16:00:00     | 1763568000   |

(Helicon is intentionally omitted: it is unscheduled and coreth maps it to no
Ethereum upgrade.)

## Phase → revm `SpecId` mapping

Source of truth: coreth `params/config_extra.go:SetEthUpgrades`
(`../avalanchego/graft/coreth/params/config_extra.go`). coreth enables the
Ethereum upgrade at the *same time* as the Avalanche phase that introduces it:

| coreth line | rule                                          | phase → SpecId          |
|-------------|-----------------------------------------------|-------------------------|
| l.37–47     | Homestead..MuirGlacier at block 0             | Launch/AP1 → `ISTANBUL` |
| l.56–58     | `c.BerlinBlock` = AP2 activation block (mainnet 1640340) | AP2 → `BERLIN`  |
| l.56–58     | `c.LondonBlock` = AP3 activation block (mainnet 3308552) | AP3 → `LONDON`  |
| l.82–84     | `c.ShanghaiTime = DurangoBlockTimestamp`      | Durango → `SHANGHAI`    |
| l.86–88     | `c.CancunTime = EtnaTimestamp`                | Etna → `CANCUN`         |

Between mapped phases the SpecId is held: AP3..Cortina = `LONDON`,
Durango..(pre-Etna) = `SHANGHAI`, Etna..Granite = `CANCUN`. coreth pins **no**
`PragueTime`/`VerkleTime` at the pinned avalanchego revision, so Fortuna and
Granite (Avalanche-only fee/consensus changes) keep `CANCUN`.

> Note: coreth maps Berlin/London by *block number* on mainnet/Fuji (the AP2/AP3
> activation blocks), not by timestamp. `AvaChainSpec` is timestamp-only (the
> reth `ForkCondition::Timestamp` model + SAE's timestamp-keyed schedule), so the
> Eth-fork entries in `ChainHardforks` use the AP2/AP3 *timestamps*. This is
> observationally identical for `revm_spec_id` selection (the spec id at the AP2
> block equals the spec id at the AP2 timestamp); it differs only if a consumer
> needs the literal Berlin/London *block height*, which the EVM executor does not
> (it keys on timestamp). Recorded as an as-built note in the M6.5 report.

## Regenerating

The timestamps come from `ava-version` (no external fetch). If avalanchego bumps
a mainnet upgrade time, update `ava_version::upgrade::mainnet_config()` and this
vector together. The phase→SpecId column changes only if coreth's
`SetEthUpgrades` gains a new Ethereum-fork mapping (e.g. a future `PragueTime`).
