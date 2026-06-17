// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

// Command rustplugin is the live arm of avalanche-rs M9.3
// (`differential::plugin_rust_in_go`): it boots a real single-node Go
// `avalanchego` tmpnet, creates a subnet + blockchain whose VM is the Rust
// `testvm_plugin` rpcchainvm guest binary, and asserts the Go node spawns the
// Rust plugin and completes the rpcchainvm v45 reverse-dial handshake.
//
// This is the env-gated Go-oracle emitter for the live two-binary interop arm
// (the canonical copy lives in the avalanche-rs repo under
// tests/differential/go-oracle/rust_plugin_handshake/; it is copied into
// ~/avalanchego to compile against the tmpnet fixture). It is NOT part of the
// avalanchego build; it runs only when invoked explicitly with the two env
// vars below.
//
// Invocation (from ~/avalanchego):
//
//	AVALANCHEGO_PATH=$HOME/avalanchego/build/avalanchego \
//	RUST_PLUGIN_PATH=<avalanche-rs>/target/debug/examples/testvm_plugin \
//	go run ./tests/rustplugin
//
// Exit code 0 = handshake observed (PASS); non-zero = FAIL with the scanned
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
	fmt.Println("PASS: Go node spawned the Rust plugin and the rpcchainvm v45 handshake was observed")
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

	log := tests.NewDefaultLogger("rustplugin")

	// Deterministic VM id for the Rust plugin. The plugin binary must be
	// installed in the node's plugin dir under this id's string form.
	vmID := ids.ID(hashing.ComputeHash256Array([]byte("avalanche-rs-rust-testvm")))

	rootDir, err := os.MkdirTemp("", "rustplugin-net-*")
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

	// Point the spawned node at our plugin dir via the env var. tmpnet passes
	// node config through `--config-file`, but avalanchego's getPluginDir only
	// honors the file value when `viper.IsSet("plugin-dir")` is true — which it
	// is NOT for the config-file path in this version, so the node silently
	// falls back to its default `$AVAGO_DATA_DIR/plugins`. The env var is a
	// viper source that DOES set IsSet, and tmpnet's ProcessRuntime does not set
	// an explicit child env (exec.Command inherits os.Environ), so the spawned
	// node inherits this and it reliably wins. The var name is
	// avago<dashes→underscores, upper> per config.EnvVarName(EnvPrefix="avago").
	const pluginDirEnv = "AVAGO_PLUGIN_DIR"
	if err := os.Setenv(pluginDirEnv, pluginDir); err != nil {
		return fmt.Errorf("set %s: %w", pluginDirEnv, err)
	}

	nodes := tmpnet.NewNodesOrPanic(1)
	network := &tmpnet.Network{
		Owner: "avalanche-rs-rust-plugin-handshake",
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
						// VersionArgs intentionally empty: the Rust plugin has no
						// `version-json` flag, so tmpnet's rpcchainvm version
						// pre-check is skipped (the handshake itself negotiates v45).
						VMID:    vmID,
						Genesis: []byte("genesis"),
					},
				},
				ValidatorIDs: tmpnet.NodesToIDs(nodes...),
			},
		},
	}

	ctx, cancel := context.WithTimeout(context.Background(), 4*time.Minute)
	defer cancel()

	// Best-effort teardown of the node process regardless of outcome.
	defer func() {
		stopCtx, stopCancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer stopCancel()
		_ = network.Stop(stopCtx)
	}()

	bootErr := tmpnet.BootstrapNewNetwork(ctx, log, network, rootDir)
	if bootErr != nil {
		// Don't fail yet: the rpcchainvm handshake happens when the chain
		// manager spawns the plugin, which can precede full chain health. Scan
		// the log below — the handshake may have completed even if bootstrap
		// timed out waiting for the chain to finish bootstrapping.
		fmt.Fprintf(os.Stderr, "BOOTSTRAP ERROR: %v\n", bootErr)
	}

	// Give the (restarted) node a moment to track the subnet, create the chain,
	// and spawn the plugin.
	time.Sleep(15 * time.Second)

	logPath := filepath.Join(nodes[0].DataDir, "logs", "main.log")
	observed, evidence, err := scanForHandshake(logPath, vmID)
	if err != nil {
		return fmt.Errorf("scan node log %q: %w (bootstrap err: %v)", logPath, err, bootErr)
	}
	if !observed {
		return fmt.Errorf("rpcchainvm v45 handshake / plugin spawn for VM %s NOT observed in %s (bootstrap err: %v)", vmID, logPath, bootErr)
	}

	fmt.Printf("VM id      : %s\n", vmID)
	fmt.Printf("plugin     : %s\n", pluginDst)
	fmt.Printf("node log   : %s\n", logPath)
	fmt.Println("evidence:")
	for _, line := range evidence {
		fmt.Printf("  %s\n", line)
	}
	return nil
}

// scanForHandshake reads the node log and returns whether the Rust plugin was
// spawned / completed the rpcchainvm handshake, plus the matching log lines.
func scanForHandshake(logPath string, vmID ids.ID) (bool, []string, error) {
	data, err := os.ReadFile(logPath)
	if err != nil {
		return false, nil, err
	}
	vmStr := vmID.String()
	// STRICT: the only trustworthy signal that the Go host spawned OUR Rust
	// plugin is a log line that names the plugin's VM id. The chain manager's
	// `creating chain {... "vmID": "<vmStr>"}` line is emitted exactly when it
	// instantiates the rpcchainvm runtime for that VM (= spawn + v45 handshake).
	// We deliberately do NOT match generic "chain"/"plugin"/"rpcchainvm" lines:
	// those appear for the primary-network P/C/X chains too and caused a false
	// positive in an earlier revision. The node-init line echoes provided flags
	// (which can embed encoded blobs) — exclude it explicitly.
	// The chain manager logs, per attempt to instantiate our chain:
	//   INFO  "creating chain"        {... "vmID": "<vmStr>"}     (always)
	//   ERROR "error creating chain"  {... "vmID": "<vmStr>"}     (only on failure)
	// To reach a *successful* "creating chain" the manager must resolve the
	// rpcchainvm factory, spawn the plugin (v45 reverse-dial handshake) and call
	// Initialize without error. So: handshake observed iff some attempt created
	// the chain with no paired error, i.e. creates > errors. The pre-restart
	// node may log a transient failure (it doesn't yet track the subnet) — that
	// is why we compare counts rather than failing on any error line.
	var evidence []string
	creates := 0
	errCreates := 0
	for _, raw := range strings.Split(string(data), "\n") {
		line := strings.TrimSpace(raw)
		if line == "" || !strings.Contains(line, vmStr) || strings.Contains(line, "initializing node") {
			continue
		}
		if len(evidence) < 25 {
			evidence = append(evidence, line)
		}
		switch {
		case strings.Contains(line, "error creating chain"):
			errCreates++
		case strings.Contains(line, "creating chain"):
			creates++
		}
	}
	return creates > errCreates, evidence, nil
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
