// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `gen-flags`: regenerate the Go flag-catalog snapshot for the
//! `golden::flag_parity` exit gate (specs 13 §25, plan M8.4).
//!
//! Drops the embedded Go emitter into the avalanchego checkout
//! (`$AVALANCHEGO_DIR`, default `../avalanchego`), runs it env-gated
//! (`AVALANCHE_RS_FLAGS_OUT`), and writes the sorted
//! `{name,type,default,deprecated,deprecation_msg}` records — with the
//! OS-dependent `fd-limit` and `NumCPU`-derived defaults pinned to symbolic
//! forms — to `crates/ava-config/tests/vectors/config/flags.json`. The dropped
//! emitter is deleted afterwards, mirroring the SAE go-oracle pattern
//! (`tests/differential/go-oracle/README.md`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

/// File name the emitter is dropped under in `<avalanchego>/config/`.
const EMITTER_FILE: &str = "avalanche_rs_gen_flags_test.go";

/// The Go emitter, verbatim (`package config` so it can call the unexported
/// `deprecateFlags`). Env-gated: a normal `go test` run skips it.
const EMITTER_GO: &str = r#"// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package config

// avalanche-rs flag-catalog snapshot emitter for the M8.4 golden::flag_parity
// test (avalanche-rs specs/13 §25, specs/12 §1.8).
//
// Dumps every flag registered by config.BuildFlagSet() (after deprecateFlags)
// as {name,type,default,deprecated,deprecation_msg}, sorted by name. The
// OS-dependent fd-limit default and the runtime-derived NumCPU defaults are
// pinned to their SYMBOLIC forms so the snapshot is host-independent.
//
// Gated behind AVALANCHE_RS_FLAGS_OUT=<output-file> so a normal `go test` run
// never executes it. The committed source-of-truth copy of this file lives in
// the avalanche-rs repo (embedded in xtask/src/gen_flags.rs); this copy is
// dropped into the avalanchego checkout to run, then deleted.

import (
	"encoding/json"
	"os"
	"os/exec"
	"sort"
	"strings"
	"testing"

	"github.com/spf13/pflag"
	"github.com/stretchr/testify/require"
)

type flagRecord struct {
	Name           string `json:"name"`
	Type           string `json:"type"`
	Default        string `json:"default"`
	Deprecated     bool   `json:"deprecated"`
	DeprecationMsg string `json:"deprecation_msg"`
}

type flagSnapshot struct {
	Provenance map[string]string `json:"_provenance"`
	Flags      []flagRecord      `json:"flags"`
}

// symbolicDefaults pins host-dependent pflag DefValue strings to symbolic
// forms (specs/13 §25 step 1): the comparison must be stable across GOOS and
// core counts.
var symbolicDefaults = map[string]string{
	"fd-limit": "DefaultFDLimit",
	"throttler-inbound-cpu-validator-alloc":              "NumCPU",
	"throttler-inbound-cpu-max-non-validator-usage":      "0.8*NumCPU",
	"throttler-inbound-cpu-max-non-validator-node-usage": "NumCPU/8",
}

func TestAvalancheRsGenFlags(t *testing.T) {
	out := os.Getenv("AVALANCHE_RS_FLAGS_OUT")
	if out == "" {
		t.Skip("AVALANCHE_RS_FLAGS_OUT not set; skipping avalanche-rs flag snapshot emit")
	}

	fs := BuildFlagSet()
	require.NoError(t, deprecateFlags(fs))

	var recs []flagRecord
	fs.VisitAll(func(f *pflag.Flag) {
		def := f.DefValue
		if sym, ok := symbolicDefaults[f.Name]; ok {
			def = sym
		}
		recs = append(recs, flagRecord{
			Name:           f.Name,
			Type:           f.Value.Type(),
			Default:        def,
			Deprecated:     f.Deprecated != "",
			DeprecationMsg: f.Deprecated,
		})
	})
	sort.Slice(recs, func(i, j int) bool { return recs[i].Name < recs[j].Name })

	commit := "unknown"
	if raw, err := exec.Command("git", "rev-parse", "HEAD").Output(); err == nil {
		commit = strings.TrimSpace(string(raw))
	}

	snap := flagSnapshot{
		Provenance: map[string]string{
			"source":    "github.com/ava-labs/avalanchego config.BuildFlagSet() + deprecateFlags (config/{flags,keys,config}.go)",
			"go_commit": commit,
			"generator": "avalanche-rs `cargo xtask gen-flags` (specs/13 §25, plan M8.4)",
			"pinned_symbolic_defaults": "fd-limit=DefaultFDLimit (32768 linux/bsd, 10240 darwin); " +
				"throttler-inbound-cpu-validator-alloc=NumCPU; " +
				"throttler-inbound-cpu-max-non-validator-usage=0.8*NumCPU; " +
				"throttler-inbound-cpu-max-non-validator-node-usage=NumCPU/8",
		},
		Flags: recs,
	}

	data, err := json.MarshalIndent(snap, "", "  ")
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(out, append(data, '\n'), 0o600))
	t.Logf("wrote %d flag records to %s", len(recs), out)
}
"#;

/// Runs the `gen-flags` regeneration end to end.
pub fn run() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask has no parent dir")?
        .to_path_buf();
    let go_dir = avalanchego_dir(&repo_root)?;
    let out = repo_root.join("crates/ava-config/tests/vectors/config/flags.json");
    fs::create_dir_all(out.parent().context("flags.json has no parent")?)?;

    let dropped = go_dir.join("config").join(EMITTER_FILE);
    fs::write(&dropped, EMITTER_GO)
        .with_context(|| format!("drop emitter at {}", dropped.display()))?;
    let status = Command::new("go")
        .args([
            "test",
            "-tags",
            "test",
            "-count=1",
            "-run",
            "TestAvalancheRsGenFlags",
            "./config/",
        ])
        .current_dir(&go_dir)
        .env("CGO_ENABLED", "1")
        .env("AVALANCHE_RS_FLAGS_OUT", &out)
        .status();
    // Always clean up the dropped emitter, even on failure.
    let cleanup = fs::remove_file(&dropped);
    let status = status.context("run `go test` (is Go on PATH?)")?;
    cleanup.with_context(|| format!("remove dropped emitter {}", dropped.display()))?;
    if !status.success() {
        bail!("go emitter failed with {status}");
    }
    println!("gen-flags: wrote {}", out.display());
    Ok(())
}

/// Resolves the avalanchego checkout: `$AVALANCHEGO_DIR`, else
/// `<repo>/../avalanchego`.
fn avalanchego_dir(repo_root: &Path) -> anyhow::Result<PathBuf> {
    let dir = std::env::var_os("AVALANCHEGO_DIR").map_or_else(
        || {
            repo_root
                .parent()
                .map(|p| p.join("avalanchego"))
                .unwrap_or_default()
        },
        PathBuf::from,
    );
    if !dir.join("config").is_dir() {
        bail!(
            "avalanchego checkout not found at {} (set AVALANCHEGO_DIR)",
            dir.display()
        );
    }
    Ok(dir)
}
