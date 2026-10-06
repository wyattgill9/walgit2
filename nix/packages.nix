{self, ...}: {
  perSystem = {
    pkgs,
    lib,
    craneLib,
    config,
    ...
  }: let
    src = lib.cleanSourceWith {
      src = ../.;
      # crane's cargo filter keeps every *.toml, which walgit-config's tests need (they
      # include_str! the root walgit.*.toml configs); walgit-proto's build.rs compiles the
      # .proto schema, which the filter would drop.
      filter = path: type:
        (craneLib.filterCargoSources path type)
        || builtins.match ".*\\.proto$" path != null;
    };

    commonArgs = {
      inherit src;
      strictDeps = true;

      # protoc builds walgit-proto; the rest cover the gix/aws/rustls C bits.
      # Native deps go here; the devshell inherits both lists.
      nativeBuildInputs = with pkgs; [protobuf pkg-config cmake perl python3];
      buildInputs = with pkgs; [];
    };

    cargoArtifacts = craneLib.buildDepsOnly commonArgs;

    # The SPA (pnpm/vite), embedded into the server binary at compile time
    # (crates/walgit-server/build.rs reads web/dist; without it, a placeholder page).
    # After changing web/pnpm-lock.yaml: set `hash` to lib.fakeHash, `nix build .#web`,
    # paste the hash it prints.
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
  in {
    packages = {
      inherit web;

      walgit = craneLib.buildPackage (
        commonArgs
        // {
          inherit cargoArtifacts;
          pname = "walgit";
          # Overriding cargoExtraArgs drops crane's default --locked; keep it.
          cargoExtraArgs = "-p walgit-cli --locked";
          # checks.test runs the suite; the package build should not run it again.
          doCheck = false;

          WALGIT_BUILD_SHA = self.shortRev or self.dirtyShortRev or "dev";

          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [pkgs.makeWrapper];
          preConfigure = ''
            mkdir -p web
            cp -a ${web} web/dist
          '';
          # `walgit serve` shells out to git (upload-pack, repack, index-pack) and git-lfs.
          postInstall = ''
            for b in walgit walgit-server; do
              wrapProgram "$out/bin/$b" \
                --prefix PATH : ${lib.makeBinPath [pkgs.git pkgs.git-lfs]}
            done
          '';

          meta = {
            description = "git hosting on an object store: smart HTTP, packfile-uri, LFS, web UI — one binary";
            mainProgram = "walgit";
            license = lib.licenses.mit;
          };
        }
      );

      default = config.packages.walgit;
    };

    # `nix flake check` runs these plus treefmt (added by the treefmt-nix module).
    # build.rs writes a placeholder web/dist when it is missing, so these compile without
    # the (separately built) web derivation.
    checks = {
      clippy = craneLib.cargoClippy (
        commonArgs
        // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--all-targets -- --deny warnings";
        }
      );

      test = craneLib.cargoNextest (
        commonArgs
        // {
          inherit cargoArtifacts;
          # git-spawning tests need git + git-lfs on PATH.
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [pkgs.git pkgs.git-lfs];
        }
      );
    };

    _module.args = {inherit commonArgs;};
  };
}
