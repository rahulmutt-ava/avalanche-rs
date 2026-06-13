// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package info

// API-parity golden-vector emitter for the avalanche-rs `ava-api` M8.23
// differential test (`tests/differential_api_parity.rs::api_parity`).
//
// This is the Go oracle. It produces REAL Go responses for the built-in
// node-level services that `ava-api` itself hosts — `info`, `admin`, and
// `health` — by constructing each service's actual reply struct and
// marshaling it through the real Go `utils/json` codec (the same
// `json.Uint32` / `json.Uint64` / `json.Float64` / `signer.ProofOfPossession`
// / `upgrade.Config` marshalers the live service uses). The reply *shape* is
// the load-bearing thing the differential test pins (field names, json.Uint*
// quoting, omitempty, RFC3339 timestamps, set ordering); the inputs are fully
// pinned fixtures so the output is deterministic.
//
// It also emits, for every node-level + chain-level service, the canonical
// method-set (the exact gorilla wire names) so the Rust test can assert
// method-set completeness (14 §14.2) against the real Go service surface.
//
// The error-response snapshots (14 §16.6) are wire-shape constants of the
// gorilla json2 codec (`utils/json` + `gorilla/rpc/v2/json2`); they are
// emitted here so the Rust dispatch shim is compared against the real Go
// codes.
//
// Gated behind `AVAX_RS_API_PARITY_OUT=<output-file>` so a normal `go test`
// run never executes it. The committed source-of-truth copy lives in
// avalanche-rs under `crates/ava-api/tests/go-oracle/`; drop it into the
// avalanchego `api/info/` package to run (it builds info/admin/health reply
// structs, all importable from there), then DELETE it:
//
//	cp crates/ava-api/tests/go-oracle/api_parity_oracle_test.go \
//	   "$AVALANCHEGO_DIR/api/info/"
//	cd "$AVALANCHEGO_DIR"
//	AVAX_RS_GO_COMMIT=$(git rev-parse HEAD) \
//	AVAX_RS_API_PARITY_OUT=/abs/crates/ava-api/tests/vectors/api/api_parity.json \
//	  go test -tags test -run TestEmitAvalancheRsAPIParity ./api/info/ -count=1 -v
//	rm "$AVALANCHEGO_DIR/api/info/api_parity_oracle_test.go"

import (
	"encoding/json"
	"net/netip"
	"os"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/ava-labs/avalanchego/api/admin"
	"github.com/ava-labs/avalanchego/api/health"
	"github.com/ava-labs/avalanchego/ids"
	"github.com/ava-labs/avalanchego/network/peer"
	avajson "github.com/ava-labs/avalanchego/utils/json"
	"github.com/ava-labs/avalanchego/utils/logging"
	"github.com/ava-labs/avalanchego/utils/set"
	"github.com/ava-labs/avalanchego/version"
	"github.com/ava-labs/avalanchego/vms/nftfx"
	"github.com/ava-labs/avalanchego/vms/platformvm/signer"
	"github.com/ava-labs/avalanchego/vms/propertyfx"
	"github.com/ava-labs/avalanchego/vms/secp256k1fx"
)

// methodCall is one recorded request/response pair: the gorilla method wire
// name, the `params[0]` args object, and the real Go reply marshaled through
// the production json codec.
type methodCall struct {
	Method   string          `json:"method"`
	Params   json.RawMessage `json:"params"`
	Response json.RawMessage `json:"response"`
}

// serviceMethods carries the canonical gorilla method-set for a service plus
// the recorded calls whose reply shapes the Rust test compares.
type serviceMethods struct {
	Service string       `json:"service"`
	Methods []string     `json:"methods"`
	Calls   []methodCall `json:"calls"`
}

// errorSnapshot pins a gorilla json2 error code + name (14 §16.6).
type errorSnapshot struct {
	Name string `json:"name"`
	Code int    `json:"code"`
}

type apiParityVectors struct {
	GoCommit string                    `json:"goCommit"`
	Emitter  string                    `json:"emitter"`
	Services map[string]serviceMethods `json:"services"`
	Errors   []errorSnapshot           `json:"errors"`
}

// fillID returns the all-`b` ids.ID (so the cb58 string is a stable fixture).
func fillID(b byte) ids.ID {
	var id ids.ID
	for i := range id {
		id[i] = b
	}
	return id
}

func fillNodeID(b byte) ids.NodeID {
	var id ids.NodeID
	for i := range id {
		id[i] = b
	}
	return id
}

func mustMarshal(t *testing.T, v any) json.RawMessage {
	t.Helper()
	raw, err := json.Marshal(v)
	require.NoError(t, err)
	return raw
}

func bytesRepeat(b byte, n int) []byte {
	out := make([]byte, n)
	for i := range out {
		out[i] = b
	}
	return out
}

func TestEmitAvalancheRsAPIParity(t *testing.T) {
	outPath := os.Getenv("AVAX_RS_API_PARITY_OUT")
	if outPath == "" {
		t.Skip("AVAX_RS_API_PARITY_OUT not set; skipping the avalanche-rs API-parity emitter")
	}
	require := require.New(t)

	// RFC3339 timestamps marshal through time.Unix in the local zone; pin UTC
	// so any time-bearing field is deterministic.
	time.Local = time.UTC

	out := apiParityVectors{
		GoCommit: os.Getenv("AVAX_RS_GO_COMMIT"),
		Emitter:  "crates/ava-api/tests/go-oracle/api_parity_oracle_test.go",
		Services: map[string]serviceMethods{},
	}
	require.NotEmpty(out.GoCommit, "set AVAX_RS_GO_COMMIT for provenance")

	// ---- pinned fixtures (must match the Rust replay exactly) -----------
	nodeID := fillNodeID(0x07)
	pop := &signer.ProofOfPossession{}
	copy(pop.PublicKey[:], bytesRepeat(0x01, 48))
	copy(pop.ProofOfPossession[:], bytesRepeat(0x02, 96))
	xChainID := fillID(0x03)
	avmID := fillID(0x08)

	// =====================================================================
	// info — 14 §3 (13 methods)
	// =====================================================================
	infoSvc := serviceMethods{
		Service: "info",
		Methods: []string{
			"getNodeVersion", "getNodeID", "getNodeIP", "getNetworkID",
			"getNetworkName", "getBlockchainID", "peers", "isBootstrapped",
			"upgrades", "uptime", "acps", "getTxFee", "getVMs",
		},
	}

	infoSvc.Calls = append(infoSvc.Calls, methodCall{
		Method: "getNodeVersion",
		Params: json.RawMessage(`{}`),
		Response: mustMarshal(t, GetNodeVersionReply{
			Version:            "avalanchego/1.14.2",
			DatabaseVersion:    version.CurrentDatabase,
			RPCProtocolVersion: avajson.Uint32(version.RPCChainVMProtocol),
			GitCommit:          "de4da4de",
			VMVersions: map[string]string{
				"avm":      "v1.14.2",
				"platform": "v1.14.2",
			},
		}),
	})

	infoSvc.Calls = append(infoSvc.Calls, methodCall{
		Method:   "getNodeID",
		Params:   json.RawMessage(`{}`),
		Response: mustMarshal(t, GetNodeIDReply{NodeID: nodeID, NodePOP: pop}),
	})

	infoSvc.Calls = append(infoSvc.Calls, methodCall{
		Method:   "getNetworkID",
		Params:   json.RawMessage(`{}`),
		Response: mustMarshal(t, GetNetworkIDReply{NetworkID: avajson.Uint32(1)}),
	})

	infoSvc.Calls = append(infoSvc.Calls, methodCall{
		Method:   "getNetworkName",
		Params:   json.RawMessage(`{}`),
		Response: mustMarshal(t, GetNetworkNameReply{NetworkName: "mainnet"}),
	})

	infoSvc.Calls = append(infoSvc.Calls, methodCall{
		Method:   "getBlockchainID",
		Params:   mustMarshal(t, GetBlockchainIDArgs{Alias: "X"}),
		Response: mustMarshal(t, GetBlockchainIDReply{BlockchainID: xChainID}),
	})

	infoSvc.Calls = append(infoSvc.Calls, methodCall{
		Method:   "isBootstrapped",
		Params:   mustMarshal(t, IsBootstrappedArgs{Chain: "P"}),
		Response: mustMarshal(t, IsBootstrappedResponse{IsBootstrapped: true}),
	})

	infoSvc.Calls = append(infoSvc.Calls, methodCall{
		Method: "uptime",
		Params: json.RawMessage(`{}`),
		Response: mustMarshal(t, UptimeResponse{
			RewardingStakePercentage:  avajson.Float64(91.5),
			WeightedAveragePercentage: avajson.Float64(98.123456),
		}),
	})

	mainnetFee := mainnetGetTxFeeResponse
	mainnetFee.TxFee = avajson.Uint64(1_000_000)
	mainnetFee.CreateAssetTxFee = avajson.Uint64(10_000_000)
	infoSvc.Calls = append(infoSvc.Calls, methodCall{
		Method:   "getTxFee",
		Params:   json.RawMessage(`{}`),
		Response: mustMarshal(t, mainnetFee),
	})

	infoSvc.Calls = append(infoSvc.Calls, methodCall{
		Method: "getVMs",
		Params: json.RawMessage(`{}`),
		Response: mustMarshal(t, GetVMsReply{
			VMs: map[ids.ID][]string{avmID: {"avm"}},
			Fxs: map[ids.ID]string{
				secp256k1fx.ID: secp256k1fx.Name,
				nftfx.ID:       nftfx.Name,
				propertyfx.ID:  propertyfx.Name,
			},
		}),
	})

	peerID := fillNodeID(0x09)
	peerInfo := peer.Info{
		IP:             netip.MustParseAddrPort("10.0.0.1:9651"),
		ID:             peerID,
		Version:        "avalanchego/1.14.2",
		UpgradeTime:    1_607_144_400,
		LastSent:       time.Date(2026, 6, 11, 12, 0, 0, 0, time.UTC),
		LastReceived:   time.Date(2026, 6, 11, 12, 0, 1, 0, time.UTC),
		ObservedUptime: avajson.Uint32(100),
		TrackedSubnets: set.Of(fillID(0x05)),
		SupportedACPs:  set.Of[uint32](23, 103, 5),
		ObjectedACPs:   set.Set[uint32]{},
	}
	infoSvc.Calls = append(infoSvc.Calls, methodCall{
		Method: "peers",
		Params: json.RawMessage(`{}`),
		Response: mustMarshal(t, PeersReply{
			NumPeers: avajson.Uint64(1),
			Peers:    []Peer{{Info: peerInfo, Benched: []string{"C"}}},
		}),
	})

	out.Services["info"] = infoSvc

	// =====================================================================
	// admin — 14 §4 (13 methods)
	// =====================================================================
	adminSvc := serviceMethods{
		Service: "admin",
		Methods: []string{
			"startCPUProfiler", "stopCPUProfiler", "memoryProfile",
			"lockProfile", "alias", "aliasChain", "getChainAliases",
			"stacktrace", "setLoggerLevel", "getLoggerLevel", "getConfig",
			"loadVMs", "dbGet",
		},
	}

	adminSvc.Calls = append(adminSvc.Calls, methodCall{
		Method:   "getChainAliases",
		Params:   mustMarshal(t, admin.GetChainAliasesArgs{Chain: xChainID.String()}),
		Response: mustMarshal(t, admin.GetChainAliasesReply{Aliases: []string{"X", xChainID.String()}}),
	})

	adminSvc.Calls = append(adminSvc.Calls, methodCall{
		Method: "getLoggerLevel",
		Params: mustMarshal(t, admin.GetLoggerLevelArgs{LoggerName: "C"}),
		Response: mustMarshal(t, admin.LoggerLevelReply{
			LoggerLevels: map[string]admin.LogAndDisplayLevels{
				"C": {LogLevel: logging.Info, DisplayLevel: logging.Info},
			},
		}),
	})

	adminSvc.Calls = append(adminSvc.Calls, methodCall{
		Method: "loadVMs",
		Params: json.RawMessage(`{}`),
		Response: mustMarshal(t, admin.LoadVMsReply{
			NewVMs:    map[ids.ID][]string{avmID: {"avm"}},
			FailedVMs: nil,
		}),
	})

	out.Services["admin"] = adminSvc

	// =====================================================================
	// health — 14 §5 (3 JSON-RPC methods; the dual GET handler is HTTP-only)
	//
	// The volatile `timestamp` / `duration` fields (02 §11.4) are normalized
	// out by the Rust replay before comparison; everything else (the
	// `{checks, healthy}` envelope, the `message` detail tag, the omitempty
	// behaviour of error / contiguousFailures / timeOfFirstFailure) is pinned.
	// =====================================================================
	healthSvc := serviceMethods{
		Service: "health",
		Methods: []string{"health", "readiness", "liveness"},
	}
	healthyReply := health.APIReply{
		Checks: map[string]health.Result{
			"c": {
				Details: json.RawMessage(`"ok"`),
			},
		},
		Healthy: true,
	}
	for _, m := range []string{"health", "readiness", "liveness"} {
		healthSvc.Calls = append(healthSvc.Calls, methodCall{
			Method:   m,
			Params:   json.RawMessage(`{}`),
			Response: mustMarshal(t, healthyReply),
		})
	}
	out.Services["health"] = healthSvc

	// =====================================================================
	// platform / avm — method-set completeness only (14 §8/§9). These
	// services live in their own crates (ava-platformvm/ava-avm) and cannot
	// be driven in-process from ava-api (the VM crates must not import
	// ava-api); their reply-shape parity is covered by the differential
	// tests inside those crates (M8.23a/M8.23b). We pin the canonical wire
	// name set here so the Rust test asserts no method went silently missing.
	// =====================================================================
	out.Services["platform"] = serviceMethods{
		Service: "platform",
		Methods: []string{
			"getHeight", "getProposedHeight", "getBalance", "getUTXOs",
			"getSubnet", "getSubnets", "getStakingAssetID", "getCurrentValidators",
			"getL1Validator", "getCurrentSupply", "sampleValidators",
			"getBlockchainStatus", "validatedBy", "validates", "getBlockchains",
			"issueTx", "getTx", "getTxStatus", "getStake", "getMinStake",
			"getTotalStake", "getRewardUTXOs", "getTimestamp", "getValidatorsAt",
			"getAllValidatorsAt", "getBlock", "getBlockByHeight", "getFeeConfig",
			"getFeeState", "getValidatorFeeConfig", "getValidatorFeeState",
		},
	}
	out.Services["avm"] = serviceMethods{
		Service: "avm",
		Methods: []string{
			"getBlock", "getBlockByHeight", "getHeight", "issueTx", "getTxStatus",
			"getTx", "getUTXOs", "getAssetDescription", "getBalance",
			"getAllBalances", "getTxFee",
		},
	}

	// =====================================================================
	// error snapshots (14 §16.6) — gorilla json2 codes.
	// =====================================================================
	out.Errors = []errorSnapshot{
		{Name: "badParams", Code: -32602},
		{Name: "unknownMethod", Code: -32601},
		{Name: "malformedJSON", Code: -32700},
		{Name: "invalidRequest", Code: -32600},
		{Name: "serverError", Code: -32000},
		// EVM JSON-RPC revert (geth) — code 3 (14 §16.6).
		{Name: "evmRevert", Code: 3},
	}

	raw, err := json.MarshalIndent(out, "", "  ")
	require.NoError(err)
	require.NoError(os.WriteFile(outPath, append(raw, '\n'), 0o644))
	t.Logf("wrote avalanche-rs API-parity vectors to %s", outPath)
}
