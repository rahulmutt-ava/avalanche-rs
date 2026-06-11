// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package metrics

// Metrics-schema golden emitter for the avalanche-rs `ava-api` M8.21 test
// (`tests/golden_metrics_names.rs::metrics_name_parity`; specs 18 §3).
//
// This is the Go oracle: it builds the REAL `api/metrics` gatherer tree the
// node builds — a root prefix gatherer (node.go `NewPrefixGatherer`), the
// process/runtime collectors under `avalanche_process` (node.go
// `initMetricsAPI`), a subsystem registry under `avalanche_network` with
// representative families (network/metrics.go `peers`, `peers_subnet`), and a
// per-chain `chain` label gatherer under `avalanche_snowman`
// (chains/manager.go wiring; snow/consensus/snowman/metrics.go
// `polls_successful` / `polls_failed`) — gathers it, and dumps the schema
// `{(name, type, sorted(label_keys))}` as JSON. Values are dropped; only the
// schema is golden. The scope is deliberately what the Rust `ava-api` crate
// can rebuild in-crate (see crates/ava-api/tests/PORTING.md); per-subsystem
// families live in their owning crates and are exercised by the full-node
// differential harness, not here.
//
// Gated behind `AVAX_RS_METRICS_SCHEMA_OUT=<output-file>` so a normal
// `go test` run never executes it. The committed source-of-truth copy lives
// in avalanche-rs under `crates/ava-api/tests/go-oracle/`; drop it into the
// avalanchego `api/metrics/` package to run:
//
//	cp crates/ava-api/tests/go-oracle/metrics_schema_oracle_test.go \
//	   "$AVALANCHEGO_DIR/api/metrics/"
//	cd "$AVALANCHEGO_DIR"
//	AVAX_RS_GO_COMMIT=$(git rev-parse HEAD) \
//	AVAX_RS_METRICS_SCHEMA_OUT=/abs/crates/ava-api/tests/vectors/api/metrics_schema.json \
//	  go test ./api/metrics/ -run TestEmitAvalancheRsMetricsSchema -count=1 -v
//	rm "$AVALANCHEGO_DIR/api/metrics/metrics_schema_oracle_test.go"

import (
	"encoding/json"
	"os"
	"runtime"
	"slices"
	"strings"
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/collectors"
	"github.com/stretchr/testify/require"
)

type emittedFamilySchema struct {
	Name      string   `json:"name"`
	Type      string   `json:"type"`
	LabelKeys []string `json:"label_keys"`
}

type emittedMetricsSchema struct {
	AvalanchegoCommit string                `json:"avalanchego_commit"`
	Emitter           string                `json:"emitter"`
	Goos              string                `json:"goos"`
	Families          []emittedFamilySchema `json:"families"`
}

func TestEmitAvalancheRsMetricsSchema(t *testing.T) {
	out := os.Getenv("AVAX_RS_METRICS_SCHEMA_OUT")
	if out == "" {
		t.Skip("AVAX_RS_METRICS_SCHEMA_OUT not set; skipping the avalanche-rs schema emitter")
	}
	require := require.New(t)

	// Root prefix gatherer (node.go:initMetricsAPI / NewPrefixGatherer).
	gatherer := NewPrefixGatherer()

	// node.go initMetricsAPI: process + Go-runtime collectors under
	// `avalanche_process` (constants.PlatformName + "_process").
	processReg, err := MakeAndRegister(gatherer, "avalanche_process")
	require.NoError(err)
	require.NoError(processReg.Register(collectors.NewProcessCollector(collectors.ProcessCollectorOpts{})))
	require.NoError(processReg.Register(collectors.NewGoCollector()))

	// Representative subsystem registry under `avalanche_network`
	// (network/metrics.go: an unlabelled gauge + a labelled gauge vec).
	networkReg, err := MakeAndRegister(gatherer, "avalanche_network")
	require.NoError(err)
	peers := prometheus.NewGauge(prometheus.GaugeOpts{
		Name: "peers",
		Help: "Number of network peers",
	})
	require.NoError(networkReg.Register(peers))
	peersSubnet := prometheus.NewGaugeVec(
		prometheus.GaugeOpts{
			Name: "peers_subnet",
			Help: "Number of peers that are validating a particular subnet",
		},
		[]string{"subnetID"},
	)
	peersSubnet.With(prometheus.Labels{"subnetID": "11111111111111111111111111111111LpoYY"}).Set(0)
	require.NoError(networkReg.Register(peersSubnet))

	// Per-chain label gatherer under `avalanche_snowman` (chains/manager.go:
	// NewLabelGatherer(ChainLabel) registered into the root prefix gatherer;
	// the chain registers its registry under its primary alias).
	snowmanGatherer := NewLabelGatherer("chain")
	require.NoError(gatherer.Register("avalanche_snowman", snowmanGatherer))
	pChainReg := prometheus.NewRegistry()
	require.NoError(snowmanGatherer.Register("P", pChainReg))
	pollsSuccessful := prometheus.NewCounter(prometheus.CounterOpts{
		Name: "polls_successful",
		Help: "Number of successful polls",
	})
	pollsFailed := prometheus.NewCounter(prometheus.CounterOpts{
		Name: "polls_failed",
		Help: "Number of failed polls",
	})
	require.NoError(pChainReg.Register(pollsSuccessful))
	require.NoError(pChainReg.Register(pollsFailed))

	families, err := gatherer.Gather()
	require.NoError(err)
	require.NotEmpty(families)

	schema := emittedMetricsSchema{
		AvalanchegoCommit: os.Getenv("AVAX_RS_GO_COMMIT"),
		Emitter:           "crates/ava-api/tests/go-oracle/metrics_schema_oracle_test.go",
		Goos:              runtime.GOOS,
		Families:          make([]emittedFamilySchema, 0, len(families)),
	}
	for _, family := range families {
		labelKeySet := make(map[string]struct{})
		for _, metric := range family.GetMetric() {
			for _, label := range metric.GetLabel() {
				labelKeySet[label.GetName()] = struct{}{}
			}
		}
		labelKeys := make([]string, 0, len(labelKeySet))
		for key := range labelKeySet {
			labelKeys = append(labelKeys, key)
		}
		slices.Sort(labelKeys)
		schema.Families = append(schema.Families, emittedFamilySchema{
			Name:      family.GetName(),
			Type:      strings.ToLower(family.GetType().String()),
			LabelKeys: labelKeys,
		})
	}
	slices.SortFunc(schema.Families, func(a, b emittedFamilySchema) int {
		return strings.Compare(a.Name, b.Name)
	})

	bytes, err := json.MarshalIndent(schema, "", "  ")
	require.NoError(err)
	require.NoError(os.WriteFile(out, append(bytes, '\n'), 0o644))
	t.Logf("wrote %d families to %s", len(schema.Families), out)
}
