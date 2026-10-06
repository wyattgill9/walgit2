{...}: {
  perSystem = {
    pkgs,
    rustToolchain,
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

          # The justfile is the task runner (test tiers, e2e, web-build).
          pkgs.just

          # The server and the test suites spawn git (upload-pack, index-pack, repack) and git-lfs.
          pkgs.git
          pkgs.git-lfs

          # `just web-build`: the SPA embedded by walgit-server.
          pkgs.nodejs_24
          pkgs.pnpm

          # Reading logs and the tree.
          pkgs.jq
          pkgs.ripgrep
          pkgs.fd
        ];

      RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
      RUSTC_WRAPPER = "sccache";
    };
  };
}
