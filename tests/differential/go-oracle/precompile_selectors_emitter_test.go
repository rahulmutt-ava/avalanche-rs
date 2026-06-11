// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package feemanager

import (
	"fmt"
	"testing"

	"github.com/ava-labs/avalanchego/graft/subnet-evm/precompile/contracts/gaspricemanager"
	"github.com/ava-labs/avalanchego/graft/subnet-evm/precompile/contracts/rewardmanager"
)

func TestM631EmitSelectors(t *testing.T) {
	for name, m := range FeeManagerABI.Methods {
		fmt.Printf("feemanager method %s id %x\n", name, m.ID)
	}
	for name, e := range FeeManagerABI.Events {
		fmt.Printf("feemanager event %s topic0 %x sig %q\n", name, e.ID, e.Sig)
	}
	for name, m := range rewardmanager.RewardManagerABI.Methods {
		fmt.Printf("rewardmanager method %s id %x\n", name, m.ID)
	}
	for name, e := range rewardmanager.RewardManagerABI.Events {
		fmt.Printf("rewardmanager event %s topic0 %x sig %q\n", name, e.ID, e.Sig)
	}
	for name, m := range gaspricemanager.GasPriceManagerABI.Methods {
		fmt.Printf("gaspricemanager method %s id %x\n", name, m.ID)
	}
	for name, e := range gaspricemanager.GasPriceManagerABI.Events {
		fmt.Printf("gaspricemanager event %s topic0 %x sig %q\n", name, e.ID, e.Sig)
	}
}
