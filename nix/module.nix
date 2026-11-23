{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.lumier;
  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    optionals
    types
    ;
in
{
  options.services.lumier = {
    enable = mkEnableOption "lumier, a museum-shaped viewer for a photo library";

    package = mkOption {
      type = types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = "The lumier package to run.";
    };

    root = mkOption {
      # A string, not a path: a path literal would be copied into the store,
      # library and all.
      type = types.str;
      example = "/srv/photos";
      description = "Library root. The service can read it but never write to it.";
    };

    manifest = mkOption {
      type = types.either types.path types.str;
      example = lib.literalExpression "./museum.yaml";
      description = ''
        The museum manifest. A path literal is copied into the store, which
        keeps the room graph under version control with the system
        configuration; a string is read from that location on disk instead,
        and edits there are picked up by `systemctl reload lumier`.
      '';
    };

    address = mkOption {
      type = types.str;
      default = "0.0.0.0";
      description = "Address to listen on. An IPv6 address goes in brackets.";
    };

    port = mkOption {
      type = types.port;
      default = 8080;
      description = "Port to listen on.";
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open the port in the firewall, so the LAN can reach it.";
    };

    rescanInterval = mkOption {
      type = types.nullOr types.ints.positive;
      default = 300;
      description = ''
        Seconds between rescans, so files added over SMB reach the walls
        without a restart. `null` turns the timer off; `systemctl reload
        lumier` still rescans on demand.
      '';
    };

    user = mkOption {
      type = types.str;
      default = "lumier";
      description = "User to run as. Created when left at the default.";
    };

    group = mkOption {
      type = types.str;
      default = "lumier";
      description = "Group to run as. Created when left at the default.";
    };

    extraGroups = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "users" ];
      description = "Supplementary groups, typically the one that may read the library.";
    };
  };

  config = mkIf cfg.enable {
    users.users = mkIf (cfg.user == "lumier") {
      lumier = {
        isSystemUser = true;
        group = cfg.group;
      };
    };
    users.groups = mkIf (cfg.group == "lumier") { lumier = { }; };

    networking.firewall.allowedTCPPorts = mkIf cfg.openFirewall [ cfg.port ];

    systemd.services.lumier = {
      description = "lumier photo museum";
      wantedBy = [ "multi-user.target" ];
      after = [
        "network.target"
        "local-fs.target"
      ];

      # The binary's default also logs every request at debug level, which
      # is more than the journal needs.
      environment.RUST_LOG = lib.mkDefault "info";

      serviceConfig = {
        ExecStart = lib.escapeShellArgs (
          [
            (lib.getExe cfg.package)
            "serve"
            "--root"
            cfg.root
            "--manifest"
            "${cfg.manifest}"
            "--bind"
            "${cfg.address}:${toString cfg.port}"
            "--web-dir"
            "${cfg.package}/share/lumier/web"
            "--cache-dir"
            "/var/cache/lumier"
          ]
          ++ optionals (cfg.rescanInterval != null) [
            "--rescan-interval"
            (toString cfg.rescanInterval)
          ]
        );
        # serve rescans the library and re-reads the manifest on SIGHUP.
        ExecReload = "${pkgs.coreutils}/bin/kill -HUP $MAINPID";
        Restart = "on-failure";

        User = cfg.user;
        Group = cfg.group;
        SupplementaryGroups = cfg.extraGroups;
        CacheDirectory = "lumier";

        # Read-only everywhere except the thumbnail cache. The library stays
        # untouched even if the server is compromised.
        ProtectSystem = "strict";
        ProtectHome = "read-only";
        ReadOnlyPaths = [ cfg.root ];
        PrivateTmp = true;
        PrivateDevices = true;
        NoNewPrivileges = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        SystemCallArchitectures = "native";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        CapabilityBoundingSet = if cfg.port < 1024 then [ "CAP_NET_BIND_SERVICE" ] else [ "" ];
        AmbientCapabilities = mkIf (cfg.port < 1024) [ "CAP_NET_BIND_SERVICE" ];
      };
    };
  };
}
