{self, ...}: {
  perSystem = {
    pkgs,
    craneLib,
    lib,
    ...
  }: let
    # Only the cargo inputs go into the rust build — vendored/ (the upstream
    # submodule) and web/ are deliberately excluded so the sandbox stays lean.
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../crates
        ../clippy.toml
        ../rust-toolchain.toml
        ../.cargo
      ];
    };

    commonArgs = {
      inherit src;
      strictDeps = true;

      # protoc builds walgit-proto; the rest cover the gix/aws/rustls C bits.
      nativeBuildInputs = with pkgs; [protobuf pkg-config cmake perl python3];
      buildInputs = with pkgs; [];
    };

    cargoArtifacts = craneLib.buildDepsOnly commonArgs;

    # The web UI (pnpm/vite) is built separately and embedded into the server
    # binary at compile time (crates/walgit-server/build.rs reads web/dist).
    # After changing web/pnpm-lock.yaml: `nix build .#web`, paste the hash it prints.
    web = pkgs.stdenv.mkDerivation (finalAttrs: {
      pname = "walgit-web";
      version = "0.1.0";
      src = lib.fileset.toSource {
        root = ../web;
        fileset = lib.fileset.unions [
          ../web/package.json
          ../web/pnpm-lock.yaml
          ../web/tsconfig.json
          ../web/vite.config.ts
          ../web/vite.sdk.config.ts
          ../web/index.html
          ../web/.oxlintrc.json
          ../web/src
          ../web/sdk
          ../web/plugins
        ];
      };
      pnpmDeps = pkgs.fetchPnpmDeps {
        inherit (finalAttrs) pname version src;
        fetcherVersion = 4;
        # After changing web/pnpm-lock.yaml, set to lib.fakeHash and re-run `nix build .#web`
        # to print the new value.
        hash = "sha256-VYzzmHKWPuWmTYwEOt/4OENmXVl1Ys7lZq44vQT404M=";
      };
      nativeBuildInputs = [pkgs.nodejs_24 pkgs.pnpm pkgs.pnpmConfigHook];
      buildPhase = ''
        runHook preBuild
        pnpm run build
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        cp -r dist "$out"
        test -f "$out/index.html" && test -f "$out/repos.js" && test -f "$out/repos.mjs"
        runHook postInstall
      '';
    });

    # `walgit serve` shells out to git (upload-pack, repack, bundle, index-pack).
    walgit = craneLib.buildPackage (commonArgs
      // {
        inherit cargoArtifacts;
        pname = "walgit";
        cargoExtraArgs = "-p walgit-cli --locked";
        doCheck = false;

        WALGIT_BUILD_SHA = self.shortRev or self.dirtyShortRev or "dev";

        nativeBuildInputs = commonArgs.nativeBuildInputs ++ [pkgs.makeWrapper];
        preConfigure = ''
          mkdir -p web
          cp -a ${web} web/dist
        '';
        postInstall = ''
          for b in walgit walgit-server; do
            wrapProgram "$out/bin/$b" \
              --prefix PATH : ${lib.makeBinPath [pkgs.git pkgs.git-lfs]}
          done
        '';

        meta = {
          description = "git hosting on an object store: smart HTTP, bundle-uri, LFS, web UI — one binary";
          mainProgram = "walgit";
          license = lib.licenses.mit;
        };
      });
  in {
    packages = {
      inherit walgit web;
      default = walgit;
    };

    # `nix flake check` runs these plus treefmt (added by the treefmt-nix module).
    checks = {
      # build.rs writes a placeholder web/dist when it's missing, so clippy/tests
      # compile without the (separately-built) web derivation.
      clippy = craneLib.cargoClippy (commonArgs
        // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--all-targets -- --deny warnings";
        });

      # git-spawning tests need git + git-lfs on PATH.
      test = craneLib.cargoNextest (commonArgs
        // {
          inherit cargoArtifacts;
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [pkgs.git pkgs.git-lfs];
        });
    };

    _module.args = {inherit commonArgs;};
  };
}
