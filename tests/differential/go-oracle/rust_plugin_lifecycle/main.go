// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

// Command rustpluginlifecycle is the live arm of avalanche-rs M9.13
// (the Go-host⇄Rust-guest leg of the four-way wire-identity matrix): it boots a
// real single-node Go `avalanchego` tmpnet, creates a subnet + blockchain whose
// VM is the Rust `testvm_plugin` rpcchainvm guest binary, lets the chain reach
// NormalOp, and asserts the Go host drives a full BuildBlock → VerifyBlock →
// AcceptBlock lifecycle over the live channel against the Rust guest.
//
// Where M9.3 (`rust_plugin_handshake`) proves only the v45 reverse-dial
// handshake + the first `VM.Initialize`, this harness proves the subsequent
// build/verify/accept *traffic* the M9.3 arm left undriven. The mechanism: the
// Rust `FixedGenesisVm` returns `PendingTxs` from `WaitForEvent` (bounded to
// MAX_BUILD_EVENTS=16), so the snowman engine's notifier drives
// Notify(PendingTxs) → buildBlocks → BuildBlock, and a single-validator subnet
// immediately accepts each built block. The Rust guest prints a
// `TESTVM-EVENT build|verify|accept` marker to stderr on each lifecycle op; the
// node copies plugin stderr verbatim into the chain log (utils/logging.(*log).Write
// bypasses the level filter), so this harness greps those markers and PASSes
// once it has observed at least one build, one verify, and one accept.
//
// This is the env-gated Go-oracle program for the live two-binary interop arm
// (the canonical copy lives in the avalanche-rs repo under
// tests/differential/go-oracle/rust_plugin_lifecycle/; it is copied into
// ~/avalanchego to compile against the tmpnet fixture). It is NOT part of the
// avalanchego build; it runs only when invoked explicitly with the two env vars
// below.
//
// Invocation (from ~/avalanchego):
//
//	HOME=$(mktemp -d) \
//	AVALANCHEGO_PATH=$HOME/avalanchego/build/avalanchego \
//	RUST_PLUGIN_PATH=<avalanche-rs>/target/debug/examples/testvm_plugin \
//	go run ./tests/rustpluginlifecycle
//
// Exit code 0 = full lifecycle observed (PASS); non-zero = FAIL with the scanned
// log tail printed for diagnosis.
package main

import (
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/ava-labs/avalanchego/ids"
	"github.com/ava-labs/avalanchego/tests"
	"github.com/ava-labs/avalanchego/tests/fixture/tmpnet"
	"github.com/ava-labs/avalanchego/utils/hashing"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "FAIL: %v\n", err)
		os.Exit(1)
	}
	fmt.Println("PASS: Go host drove a full BuildBlock/VerifyBlock/AcceptBlock lifecycle against the Rust plugin over the live rpcchainvm channel")
}

func run() error {
	goPath := os.Getenv("AVALANCHEGO_PATH")
	if goPath == "" {
		return fmt.Errorf("AVALANCHEGO_PATH unset (expected a Go avalanchego binary, rpcchainvm protocol 45)")
	}
	pluginSrc := os.Getenv("RUST_PLUGIN_PATH")
	if pluginSrc == "" {
		return fmt.Errorf("RUST_PLUGIN_PATH unset (expected the built Rust testvm_plugin binary)")
	}
	if _, err := os.Stat(pluginSrc); err != nil {
		return fmt.Errorf("RUST_PLUGIN_PATH %q does not exist: %w", pluginSrc, err)
	}

	log := tests.NewDefaultLogger("rustpluginlifecycle")

	// Deterministic VM id for the Rust plugin (identical to the M9.3 handshake
	// harness). The plugin binary must be installed in the node's plugin dir
	// under this id's string form.
	vmID := ids.ID(hashing.ComputeHash256Array([]byte("avalanche-rs-rust-testvm")))

	rootDir, err := os.MkdirTemp("", "rustplugin-lifecycle-*")
	if err != nil {
		return fmt.Errorf("create network root dir: %w", err)
	}
	pluginDir := filepath.Join(rootDir, "plugins")
	if err := os.MkdirAll(pluginDir, 0o755); err != nil {
		return fmt.Errorf("create plugin dir: %w", err)
	}
	pluginDst := filepath.Join(pluginDir, vmID.String())
	if err := copyExecutable(pluginSrc, pluginDst); err != nil {
		return fmt.Errorf("install Rust plugin into plugin dir: %w", err)
	}
	log.Info("installed Rust plugin")

	// Point the spawned node at our plugin dir via the env var. See the M9.3
	// rust_plugin_handshake harness / go-oracle README for why this must be the
	// AVAGO_PLUGIN_DIR *env* var (viper IsSet) and not a config-file flag.
	const pluginDirEnv = "AVAGO_PLUGIN_DIR"
	if err := os.Setenv(pluginDirEnv, pluginDir); err != nil {
		return fmt.Errorf("set %s: %w", pluginDirEnv, err)
	}

	nodes := tmpnet.NewNodesOrPanic(1)
	network := &tmpnet.Network{
		Owner: "avalanche-rs-rust-plugin-lifecycle",
		Nodes: nodes,
		DefaultRuntimeConfig: tmpnet.NodeRuntimeConfig{
			Process: &tmpnet.ProcessRuntimeConfig{
				AvalancheGoPath: goPath,
				PluginDir:       pluginDir,
			},
		},
		Subnets: []*tmpnet.Subnet{
			{
				Name: "rustvm",
				Chains: []*tmpnet.Chain{
					{
						VMID:    vmID,
						Genesis: []byte("genesis"),
					},
				},
				ValidatorIDs: tmpnet.NodesToIDs(nodes...),
			},
		},
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()

	// Best-effort teardown of the node process regardless of outcome.
	defer func() {
		stopCtx, stopCancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer stopCancel()
		_ = network.Stop(stopCtx)
	}()

	bootErr := tmpnet.BootstrapNewNetwork(ctx, log, network, rootDir)
	if bootErr != nil {
		// Don't fail yet: the lifecycle traffic happens once the chain reaches
		// NormalOp, which can lag full network health. Poll the logs below.
		fmt.Fprintf(os.Stderr, "BOOTSTRAP ERROR: %v\n", bootErr)
	}

	logsDir := filepath.Join(nodes[0].DataDir, "logs")

	// Poll for the lifecycle markers. After NormalOp the bounded build loop
	// runs quickly; give it a generous window (incl. the tmpnet sybil restart).
	deadline := time.Now().Add(90 * time.Second)
	var builds, verifies, accepts int
	var evidence []string
	for time.Now().Before(deadline) {
		builds, verifies, accepts, evidence, err = scanForLifecycle(logsDir)
		if err == nil && builds > 0 && verifies > 0 && accepts > 0 {
			break
		}
		time.Sleep(3 * time.Second)
	}
	if err != nil {
		return fmt.Errorf("scan node logs %q: %w (bootstrap err: %v)", logsDir, err, bootErr)
	}
	if builds == 0 || verifies == 0 || accepts == 0 {
		for _, line := range evidence {
			fmt.Fprintf(os.Stderr, "  evidence: %s\n", line)
		}
		return fmt.Errorf("did not observe a full lifecycle for VM %s in %s: build=%d verify=%d accept=%d (need >=1 each; bootstrap err: %v)",
			vmID, logsDir, builds, verifies, accepts, bootErr)
	}

	fmt.Printf("VM id      : %s\n", vmID)
	fmt.Printf("plugin     : %s\n", pluginDst)
	fmt.Printf("logs dir   : %s\n", logsDir)
	fmt.Printf("lifecycle  : build=%d verify=%d accept=%d (all driven over the live rpcchainvm channel)\n", builds, verifies, accepts)
	fmt.Println("evidence:")
	for _, line := range evidence {
		fmt.Printf("  %s\n", line)
	}
	return nil
}

// scanForLifecycle reads every *.log file under logsDir and counts the Rust
// guest's `TESTVM-EVENT build|verify|accept` stderr markers (copied verbatim
// into the chain log by the node). Each marker proves the corresponding
// `proto/vm` RPC was driven by the Go host and served by the Rust guest.
func scanForLifecycle(logsDir string) (builds, verifies, accepts int, evidence []string, err error) {
	entries, err := os.ReadDir(logsDir)
	if err != nil {
		return 0, 0, 0, nil, err
	}
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".log") {
			continue
		}
		data, rerr := os.ReadFile(filepath.Join(logsDir, e.Name()))
		if rerr != nil {
			continue
		}
		for _, raw := range strings.Split(string(data), "\n") {
			idx := strings.Index(raw, "TESTVM-EVENT ")
			if idx < 0 {
				continue
			}
			ev := strings.TrimSpace(raw[idx:])
			switch {
			case strings.HasPrefix(ev, "TESTVM-EVENT build"):
				builds++
			case strings.HasPrefix(ev, "TESTVM-EVENT verify"):
				verifies++
			case strings.HasPrefix(ev, "TESTVM-EVENT accept"):
				accepts++
			}
			if len(evidence) < 12 {
				evidence = append(evidence, ev)
			}
		}
	}
	return builds, verifies, accepts, evidence, nil
}

func copyExecutable(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(dst, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o755)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		_ = out.Close()
		return err
	}
	return out.Close()
}
