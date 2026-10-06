{inputs, ...}: {
  perSystem = {system, ...}: let
    pkgs = import inputs.nixpkgs {
      inherit system;
      overlays = [inputs.rust-overlay.overlays.default];
    };

    rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;

    # Default linker on purpose (no wild): rustc's self-contained lld on Linux, ld64 on macOS.
    craneLib = (inputs.crane.mkLib pkgs).overrideToolchain (_: rustToolchain);
  in {
    _module.args = {
      inherit pkgs rustToolchain craneLib;
    };
  };
}
