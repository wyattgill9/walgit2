# rust-nix-template

Rust workspace. Nix provides the toolchain. Cargo runs the dev loop. Nix builds reproducibly.

## Setup

Needs [Nix](https://nixos.org/download) with flakes.

```bash
git clone https://github.com/your-org/your-repo
cd your-repo
direnv allow   # or: nix develop
```

## Commands

```bash
cargo build          # dev loop, incremental
cargo nextest run    # tests
nix build            # hermetic, sandboxed
nix flake check      # clippy, tests, formatting
nix fmt              # rust, nix, toml
```

Run Cargo inside the shell. Nix only provides the environment. Do not route `cargo check` through `nix build`. That discards incremental compilation.

## Layout

```
flake.nix              # imports nix/ modules
rust-toolchain.toml    # Rust version pin, single source of truth
crates/cli/            # default crate
nix/
  toolchain.nix        # rust-overlay + crane
  packages.nix         # crane builds
  devshell.nix         # dev shell
  fmt.nix              # treefmt
```

## Common changes

**Rust version** — edit `rust-toolchain.toml`. Keep `rust-src`. rust-analyzer needs it.

**New crate** — create `crates/my-crate/`, add it to `members` in the root `Cargo.toml`, then add `my-crate = mkCrate "my-crate";` to `nix/packages.nix`.

**Native library** — add it to `buildInputs` in `nix/packages.nix` (plus `pkg-config` in `nativeBuildInputs` if the crate's build script needs it). The devshell inherits both lists.

**Binary cache** — set `nixConfig.extra-substituters` and `extra-trusted-public-keys` in `flake.nix`.

## Build model

`buildDepsOnly` builds `cargoArtifacts`, and rebuilds only when `Cargo.lock` changes. `buildPackage` then builds the binary. A source change rebuilds only the second derivation.
