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
          # cargo-llvm-cov is flagged `broken` in nixos-25.11; allowBroken lets
          # the dev shell evaluate. The build itself is fixed up below
          # (doCheck = false). Revisit when the nixpkgs pin is bumped.
          config.allowBroken = true;
        };

        # Single source of truth for the Rust version: rust-toolchain.toml.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        # Nightly toolchain — used ONLY by the fuzz dev shell (`devShells.fuzz`),
        # never by `devShells.default`. cargo-fuzz/libfuzzer-sys need a nightly
        # rustc for `-Zsanitizer`/sancov and `-Zbuild-std`; everything else in
        # the repo stays on the pinned stable above. The nightly date is
        # resolved against the rust-overlay input pinned in flake.lock, so it is
        # reproducible until that input is bumped.
        #   rust-src        — required by cargo-fuzz's default `-Zbuild-std`
        #   llvm-tools-preview — sanitizer runtime / coverage tooling
        fuzzRustToolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain:
          toolchain.default.override {
            extensions = [ "rust-src" "llvm-tools-preview" ];
          });

        # cargo-llvm-cov integration tests require Rust profiler_builtins for
        # coverage-instrumented target builds. On nixpkgs/darwin this can fail with:
        #   can't find crate for `profiler_builtins`
        # The binary is still usable with a Rust toolchain that has profiler runtime support.
        cargo-llvm-cov = pkgs.cargo-llvm-cov.overrideAttrs (_: {
          doCheck = false;
        });

        # Dev shell parametrized by the Rust toolchain so the default (stable)
        # and fuzz (nightly) shells share one package set and differ only in the
        # compiler on PATH.
        mkDevShell = toolchain: pkgs.mkShell {
          packages = [
            # Rust toolchain (rustc, cargo, clippy, rustfmt, rust-src, llvm-tools)
            toolchain
          ] ++ (with pkgs; [
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
          ]);

          # rocksdb/firewood/secp256k1 builds find libclang via this var.
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          shellHook = ''
            export PATH="$PWD/scripts:$PWD/bin:$PATH"
          '';
        };
      in
      {
        # Default shell: pinned stable toolchain for build/test/lint/everything.
        devShells.default = mkDevShell rustToolchain;

        # Fuzz shell: identical package set but with the nightly toolchain on
        # PATH so `cargo fuzz build/run` works. Entered explicitly by the fuzz
        # Task targets (NIX_DEV_SHELL=fuzz; see scripts/nix_run.sh); the default
        # shell never sees nightly.
        devShells.fuzz = mkDevShell fuzzRustToolchain;
      });
}
