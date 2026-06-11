// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package feemanager

import (
	"encoding/json"
	"fmt"
	"math/big"
	"testing"

	"github.com/ava-labs/libevm/common"
	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/graft/subnet-evm/commontype"
	"github.com/ava-labs/avalanchego/graft/subnet-evm/precompile/contracts/gaspricemanager"
	"github.com/ava-labs/avalanchego/graft/subnet-evm/precompile/contracts/rewardmanager"
)

func hexh(h common.Hash) string { return h.Hex() }
func hexb(b []byte) string      { return fmt.Sprintf("0x%x", b) }
func hexhs(hs []common.Hash) []string {
	out := make([]string, len(hs))
	for i, h := range hs {
		out[i] = h.Hex()
	}
	return out
}

func TestM631EmitGoldens(t *testing.T) {
	caller := common.HexToAddress("0x1111111111111111111111111111111111111111")

	feeCfg := commontype.FeeConfig{
		GasLimit:                 big.NewInt(8_000_000),
		TargetBlockRate:          2,
		MinBaseFee:               big.NewInt(25_000_000_000),
		TargetGas:                big.NewInt(15_000_000),
		BaseFeeChangeDenominator: big.NewInt(36),
		MinBlockGasCost:          big.NewInt(0),
		MaxBlockGasCost:          big.NewInt(1_000_000),
		BlockGasCostStep:         big.NewInt(200_000),
	}
	oldFeeCfg := commontype.FeeConfig{
		GasLimit:                 big.NewInt(1),
		TargetBlockRate:          1,
		MinBaseFee:               big.NewInt(1),
		TargetGas:                big.NewInt(1),
		BaseFeeChangeDenominator: big.NewInt(1),
		MinBlockGasCost:          big.NewInt(0),
		MaxBlockGasCost:          big.NewInt(10),
		BlockGasCostStep:         big.NewInt(1),
	}
	setFeeCalldata, err := PackSetFeeConfig(feeCfg)
	require.NoError(t, err)
	getFeeOut, err := PackGetFeeConfigOutput(feeCfg)
	require.NoError(t, err)
	feeTopics, feeData, err := PackFeeConfigChangedEvent(caller, oldFeeCfg, feeCfg)
	require.NoError(t, err)
	lcaOut, err := PackGetFeeConfigLastChangedAtOutput(big.NewInt(7))
	require.NoError(t, err)

	rewardAddr := common.HexToAddress("0x2222222222222222222222222222222222222222")
	oldReward := common.HexToAddress("0x3333333333333333333333333333333333333333")
	setRewardCalldata, err := rewardmanager.PackSetRewardAddress(rewardAddr)
	require.NoError(t, err)
	curRewardOut, err := rewardmanager.PackCurrentRewardAddressOutput(rewardAddr)
	require.NoError(t, err)
	areAllowedOut, err := rewardmanager.PackAreFeeRecipientsAllowedOutput(true)
	require.NoError(t, err)
	racTopics, racData, err := rewardmanager.PackRewardAddressChangedEvent(caller, oldReward, rewardAddr)
	require.NoError(t, err)
	fraTopics, fraData, err := rewardmanager.PackFeeRecipientsAllowedEvent(caller)
	require.NoError(t, err)
	rdTopics, rdData, err := rewardmanager.PackRewardsDisabledEvent(caller)
	require.NoError(t, err)

	gpCfg := commontype.GasPriceConfig{
		ValidatorTargetGas: false,
		TargetGas:          2_000_000,
		StaticPricing:      false,
		MinGasPrice:        1_000_000_000,
		TimeToDouble:       60,
	}
	gpOld := commontype.DefaultGasPriceConfig()
	setGpCalldata, err := gaspricemanager.PackSetGasPriceConfig(gpCfg)
	require.NoError(t, err)
	getGpOut, err := gaspricemanager.PackGetGasPriceConfigOutput(gpCfg)
	require.NoError(t, err)
	gpTopics, gpData, err := gaspricemanager.PackGasPriceConfigUpdatedEvent(caller, gpOld, gpCfg)
	require.NoError(t, err)
	gpLcaOut, err := gaspricemanager.PackGetGasPriceConfigLastChangedAtOutput(big.NewInt(7))
	require.NoError(t, err)

	golden := map[string]any{
		"source": "go-oracle subnet-evm precompile/contracts @ in-repo graft; emitter m631_golden_test.go",
		"caller": caller.Hex(),
		"feemanager": map[string]any{
			"address":                           ContractAddress.Hex(),
			"set_fee_config_calldata":           hexb(setFeeCalldata),
			"get_fee_config_output":             hexb(getFeeOut),
			"get_last_changed_at_output_block7": hexb(lcaOut),
			"event_topics":                      hexhs(feeTopics),
			"event_data":                        hexb(feeData),
			"set_fee_config_gas":                SetFeeConfigGasCost,
			"get_fee_config_gas":                GetFeeConfigGasCost,
			"get_last_changed_at_gas":           GetLastChangedAtGasCost,
			"event_gas":                         FeeConfigChangedEventGasCost,
			"field_keys":                        []string{hexh(common.Hash{1}), hexh(common.Hash{2}), hexh(common.Hash{3}), hexh(common.Hash{4}), hexh(common.Hash{5}), hexh(common.Hash{6}), hexh(common.Hash{7}), hexh(common.Hash{8})},
			"last_changed_at_key":               hexh(common.Hash{'l', 'c', 'a'}),
			"stored_words": []string{
				hexh(common.BigToHash(feeCfg.GasLimit)),
				hexh(common.BigToHash(new(big.Int).SetUint64(feeCfg.TargetBlockRate))),
				hexh(common.BigToHash(feeCfg.MinBaseFee)),
				hexh(common.BigToHash(feeCfg.TargetGas)),
				hexh(common.BigToHash(feeCfg.BaseFeeChangeDenominator)),
				hexh(common.BigToHash(feeCfg.MinBlockGasCost)),
				hexh(common.BigToHash(feeCfg.MaxBlockGasCost)),
				hexh(common.BigToHash(feeCfg.BlockGasCostStep)),
			},
		},
		"rewardmanager": map[string]any{
			"address":                                rewardmanager.ContractAddress.Hex(),
			"set_reward_address_calldata":            hexb(setRewardCalldata),
			"current_reward_address_output":          hexb(curRewardOut),
			"are_fee_recipients_allowed_output_true": hexb(areAllowedOut),
			"reward_address_changed_topics":          hexhs(racTopics),
			"reward_address_changed_data":            hexb(racData),
			"fee_recipients_allowed_topics":          hexhs(fraTopics),
			"fee_recipients_allowed_data":            hexb(fraData),
			"rewards_disabled_topics":                hexhs(rdTopics),
			"rewards_disabled_data":                  hexb(rdData),
			"allow_fee_recipients_gas":               rewardmanager.AllowFeeRecipientsGasCost,
			"are_fee_recipients_allowed_gas":         rewardmanager.AreFeeRecipientsAllowedGasCost,
			"current_reward_address_gas":             rewardmanager.CurrentRewardAddressGasCost,
			"disable_rewards_gas":                    rewardmanager.DisableRewardsGasCost,
			"set_reward_address_gas":                 rewardmanager.SetRewardAddressGasCost,
			"fee_recipients_allowed_event_gas":       rewardmanager.FeeRecipientsAllowedEventGasCost,
			"reward_address_changed_event_gas":       rewardmanager.RewardAddressChangedEventGasCost,
			"rewards_disabled_event_gas":             rewardmanager.RewardsDisabledEventGasCost,
			"reward_address_storage_key":             hexh(common.Hash{'r', 'a', 's', 'k'}),
			"allow_fee_recipients_value":             hexh(common.Hash{'a', 'f', 'r', 'a', 'v'}),
			"blackhole_value":                        hexh(common.BytesToHash(common.HexToAddress("0x0100000000000000000000000000000000000000").Bytes())),
			"stored_reward_value":                    hexh(common.BytesToHash(rewardAddr.Bytes())),
		},
		"gaspricemanager": map[string]any{
			"address":                           gaspricemanager.ContractAddress.Hex(),
			"set_gas_price_config_calldata":     hexb(setGpCalldata),
			"get_gas_price_config_output":       hexb(getGpOut),
			"get_last_changed_at_output_block7": hexb(gpLcaOut),
			"event_topics":                      hexhs(gpTopics),
			"event_data":                        hexb(gpData),
			"packed_config_word":                hexh(gpCfg.Pack()),
			"packed_default_word":               hexh(gpOld.Pack()),
		},
	}
	out, err := json.MarshalIndent(golden, "", "  ")
	require.NoError(t, err)
	fmt.Println("M631GOLDEN_BEGIN")
	fmt.Println(string(out))
	fmt.Println("M631GOLDEN_END")
}
