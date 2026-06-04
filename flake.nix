{
  # avalanche-rs development environment (specs/01-development-environment.md §3).
  #
  # To use:
  #  - install nix: `./scripts/run_task.sh install-nix`
  #  - run `nix develop` or use direnv (see CONTRIBUTING.md / .envrc)
  #
  # Single source of truth for the Rust version is rust-toolchain.toml, which
  # rust-overlay reads directly — there is exactly one version source.
  description = "avalanche-rs development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    let
      # Same supported systems as the avalanchego Go flake.
      allSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
    in
    flake-utils.lib.eachSystem allSystems (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Single source of truth for the Rust version: rust-toolchain.toml.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            # Pinned Rust toolchain (rustc, cargo, clippy, rustfmt, rust-src, llvm-tools)
            rustToolchain

            # Cargo tooling
            cargo-nextest        # canonical test runner (mirrors `go test`)
            cargo-deny           # dependency policy: licenses/bans/advisories/sources
            cargo-audit          # RustSec advisory scan
            cargo-llvm-cov       # coverage (mirrors -coverprofile/-covermode)
            cargo-machete        # unused-dependency detection
            cargo-fuzz           # libfuzzer targets (specs/02 §8)
            taplo                # TOML formatter/linter for Cargo manifests
            just                 # optional convenience runner (Taskfile is canonical)

            # Build / FFI requirements (rocksdb, firewood, blst, secp256k1, rustls).
            # clang/libclang + cmake are required by rust-rocksdb and firewood
            # (M1); blst/secp256k1 need only the C/C++ toolchain.
            clang
            llvmPackages.libclang
            cmake
            pkg-config
            openssl
            git

            # Bazel
            bazelisk
            (runCommand "bazel" {} ''mkdir -p $out/bin && ln -s ${bazelisk}/bin/bazelisk $out/bin/bazel'')
            buildifier           # format BUILD.bazel files

            # Task runner (canonical entrypoint)
            go-task

            # Protobuf (sources shared with Go) + lint/breaking checks
            protobuf             # provides protoc
            buf

            # Golden-vector extraction runs a Go program against the pinned
            # avalanchego tree (tools/extract-vectors; specs/02 §6.2).
            go

            # Monitoring / kube (kept from the Go flake)
            prometheus
            promtail
            kubectl
            k9s
            kind
            kubernetes-helm

            # Linters / misc
            shellcheck
            yamlfmt
            actionlint
            jq
            ripgrep
            solc                 # solidity compiler (EVM test contracts)
            s5cmd                # rapid S3 interactions for reexec datasets
          ];

          # rocksdb/firewood/secp256k1 builds find libclang via this var.
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          shellHook = ''
            export PATH="$PWD/scripts:$PWD/bin:$PATH"
            # Faster, reproducible incremental builds
            export CARGO_HOME="''${CARGO_HOME:-$PWD/.cargo-home}"
          '';
        };
      });
}
