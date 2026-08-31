{...}: {
  perSystem = {
    pkgs,
    rustToolchain,
    lib,
    commonArgs,
    ...
  }: {
    devShells.default = pkgs.mkShell {
      inherit (commonArgs) buildInputs;

      nativeBuildInputs =
        commonArgs.nativeBuildInputs
        ++ [
          rustToolchain
          pkgs.lldb
          pkgs.sccache
          pkgs.cargo-nextest
          # walgit: web build + git server + dev tooling
          pkgs.just
          pkgs.git
          pkgs.git-lfs
          pkgs.nodejs_24
          pkgs.pnpm
          pkgs.jq
          pkgs.ripgrep
          pkgs.fd
        ]
        ++ lib.optionals pkgs.stdenv.isLinux [pkgs.wild];

      RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
      RUSTC_WRAPPER = "sccache";

      shellHook = lib.optionalString pkgs.stdenv.isLinux ''
        export RUSTFLAGS="''${RUSTFLAGS:-} -C link-arg=-fuse-ld=wild"
      '';
    };
  };
}
