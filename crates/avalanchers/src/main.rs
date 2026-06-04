// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `avalanchers` binary entrypoint.
//!
//! Skeleton bootstrapped in milestone M0 (plan/M0-foundations.md, task M0.1).
//! It must always compile and answer `--version` / `--help`; chains, APIs, and
//! config flags are wired in as their crates land in later milestones.

#![forbid(unsafe_code)]

use clap::Parser;

/// Local build identity reported by `--version`, in `client/maj.min.patch` form.
///
/// This is the *local CLI* identity (`avalanchers/...`). The *wire/P2P* client
/// string this node advertises during the handshake stays `avalanchego` for
/// drop-in interop — that is a separate constant (`ava_version::CLIENT`, see
/// specs/26-versioning-and-compatibility.md and specs/03-core-primitives.md §5.1).
// TODO(M0.22): source the numeric version from `ava_version::CURRENT` once
// `ava-version` exists, keeping the local `avalanchers/` prefix.
const VERSION: &str = concat!("avalanchers/", env!("CARGO_PKG_VERSION"));

/// Command-line arguments for the node.
#[derive(Parser, Debug)]
#[command(
    name = "avalanchers",
    about = "Avalanche node (Rust) — drop-in replacement for avalanchego.",
    disable_version_flag = true
)]
struct Args {
    /// Print version information and exit.
    #[arg(short = 'V', long)]
    version: bool,
}

fn main() {
    let args = Args::parse();
    if args.version {
        println!("{VERSION}");
        return;
    }
}
