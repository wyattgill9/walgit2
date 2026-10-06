{self, ...}: {
  # services.walgit: one walgit host on NixOS. Secrets (store keys, token values, oidc
  # secrets) stay out of the Nix store: put them in `environmentFile` and point the
  # config's `*_env` keys / WALGIT__ overrides at them.
  flake.nixosModules.default = {
    config,
    lib,
    pkgs,
    ...
  }: let
    cfg = config.services.walgit;
    configFile = (pkgs.formats.toml {}).generate "walgit.toml" cfg.settings;
  in {
    options.services.walgit = {
      enable = lib.mkEnableOption "walgit, git hosting on an object store";

      package = lib.mkOption {
        type = lib.types.package;
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.walgit;
        description = "The walgit package (the web UI is embedded in the binary).";
      };

      settings = lib.mkOption {
        type = (pkgs.formats.toml {}).type;
        default = {};
        description = "walgit.toml as Nix; every key is documented in walgit.example.toml.";
      };

      environmentFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        example = "/run/secrets/walgit.env";
        description = "KEY=value file with AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, token values, ….";
      };
    };

    config = lib.mkIf cfg.enable {
      # The cache lives in the state directory; wiping it loses only warmth.
      services.walgit.settings.cache.dir = lib.mkDefault "/var/lib/walgit";

      systemd.services.walgit = {
        description = "walgit";
        wantedBy = ["multi-user.target"];
        after = ["network-online.target"];
        wants = ["network-online.target"];
        serviceConfig = {
          ExecStart = "${lib.getExe cfg.package} serve --config ${configFile}";
          EnvironmentFile = lib.optional (cfg.environmentFile != null) cfg.environmentFile;
          DynamicUser = true;
          StateDirectory = "walgit";
          # Binding :443 with in-process TLS.
          AmbientCapabilities = ["CAP_NET_BIND_SERVICE"];
          CapabilityBoundingSet = ["CAP_NET_BIND_SERVICE"];
          # SIGTERM starts the two-phase drain (D31); give it the 30 s phase 1 plus drain_timeout.
          TimeoutStopSec = "90s";
          Restart = "always";
          NoNewPrivileges = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
        };
      };
    };
  };
}
