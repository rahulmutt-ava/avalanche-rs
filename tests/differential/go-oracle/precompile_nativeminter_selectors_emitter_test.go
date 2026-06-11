// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package nativeminter

import (
	"fmt"
	"testing"
)

func TestM631NativeMinterSelectors(t *testing.T) {
	for name, m := range NativeMinterABI.Methods {
		fmt.Printf("nativeminter method %s id %x\n", name, m.ID)
	}
	for name, e := range NativeMinterABI.Events {
		fmt.Printf("nativeminter event %s topic0 %x\n", name, e.ID)
	}
	fmt.Printf("nativeminter address %s mintgas %d eventgas %d\n", ContractAddress.Hex(), MintGasCost, NativeCoinMintedEventGasCost)
}
