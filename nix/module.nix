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

    marking = {
      enable = mkEnableOption ''
        deletion marks. The server records marks and restore requests in its
        state directory, and a nightly `lumier purge` moves marked files into
        the trash and deletes them after {option}`keepDays`. The server itself
        still cannot write to the library'';

      trashDir = mkOption {
        type = types.str;
        example = "/srv/trash";
        description = ''
          Where trashed files go. Must be on the library's filesystem, outside
          the library, so that moving a file there is a rename.
        '';
      };

      keepDays = mkOption {
        type = types.ints.positive;
        default = 30;
        description = "Days a file stays in the trash before the purge deletes it.";
      };

      limit = mkOption {
        type = types.ints.positive;
        default = 500;
        description = ''
          The purge moves nothing if more files than this are marked, as a
          guard against marks nobody meant. Run `lumier purge --no-limit` by
          hand when that many really should go.
        '';
      };

      schedule = mkOption {
        type = types.str;
        default = "*-*-* 04:00:00";
        description = "When the purge runs, as a systemd calendar expression.";
      };

      user = mkOption {
        type = types.str;
        default = "root";
        example = "photos";
        description = ''
          User the purge runs as. It moves library files, so it needs write
          access to the library and the trash; the owner of the files (the
          Samba user, say) keeps ownership consistent better than root does.
        '';
      };
    };
  };

  config = mkIf cfg.enable {
    systemd.tmpfiles.settings.lumier = mkIf cfg.marking.enable {
      ${cfg.marking.trashDir}.d = {
        user = cfg.marking.user;
        group = cfg.group;
        mode = "0750";
      };
    };

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
          ++ optionals cfg.marking.enable [
            "--state-dir"
            "/var/lib/lumier"
            "--trash-dir"
            cfg.marking.trashDir
            "--trash-days"
            (toString cfg.marking.keepDays)
          ]
        );
        # serve rescans the library and re-reads the manifest on SIGHUP.
        ExecReload = "${pkgs.coreutils}/bin/kill -HUP $MAINPID";
        Restart = "on-failure";

        User = cfg.user;
        Group = cfg.group;
        SupplementaryGroups = cfg.extraGroups;
        CacheDirectory = "lumier";
        # Marks and restore requests: the one place the server writes besides
        # its cache. Shared with the purge through the group, so files are
        # made group-writable.
        StateDirectory = mkIf cfg.marking.enable "lumier";
        StateDirectoryMode = "0770";
        UMask = "0007";

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

    systemd.services.lumier-purge = mkIf cfg.marking.enable {
      description = "lumier: move marked files to the trash and empty it";
      after = [ "local-fs.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = lib.escapeShellArgs [
          (lib.getExe cfg.package)
          "purge"
          "--root"
          cfg.root
          "--trash-dir"
          cfg.marking.trashDir
          "--state-dir"
          "/var/lib/lumier"
          "--trash-days"
          (toString cfg.marking.keepDays)
          "--limit"
          (toString cfg.marking.limit)
        ];
        # Have the server rescan, so moved files leave the walls and the
        # trash list now. "+" runs it with full privileges whatever User is.
        ExecStartPost = "+${config.systemd.package}/bin/systemctl reload lumier.service";
        User = cfg.marking.user;
        SupplementaryGroups = [ cfg.group ];
        UMask = "0007";

        # Not ProtectSystem = "strict" with ReadWritePaths: each such path
        # becomes a bind mount of its own, and rename(2) between two mounts
        # fails with EXDEV even on one filesystem, which is every move this
        # unit makes.
        ProtectSystem = "full";
        PrivateTmp = true;
        PrivateDevices = true;
        PrivateNetwork = true;
        NoNewPrivileges = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictNamespaces = true;
        LockPersonality = true;
      };
    };

    systemd.timers.lumier-purge = mkIf cfg.marking.enable {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = cfg.marking.schedule;
        # Catch up on a run missed while the machine was off.
        Persistent = true;
      };
    };
  };
}
