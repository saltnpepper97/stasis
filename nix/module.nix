{ package }:
{ config, lib, pkgs, ... }:

let
  inherit (lib)
    mkEnableOption
    mkPackageOption
    mkIf
    mkMerge
    getExe
    mkOption
    types
    escapeShellArgs
    optionals
    literalExpression
    ;

  cfg = config.services.stasis;

  defaultArgs = optionals (cfg.extraConfig != null) [
    "-c"
    "/etc/stasis/stasis.rune"
  ];

  # Packages added to the service PATH (systemd.user.services.<name>.path expects packages).
  # Include pulseaudio so `pactl` is available for media detection on PipeWire Pulse setups.
  baseServicePathPkgs = with pkgs; [
    bashInteractive
    coreutils
    libnotify
    systemd
    pulseaudio
  ];
in
{
  options = {
    services.stasis = {
      enable = mkEnableOption "Stasis, a lightweight, feature rich Wayland idle manager written in Rust";

      package = mkPackageOption { stasis = package; } "stasis" { };

      extraConfig = mkOption {
        type = types.nullOr types.lines;
        default = null;
        description = ''
          The literal contents of the Stasis configuration file.
          If set, Nix will write this text to:
          `/etc/stasis/stasis.rune`.
        '';
        example = literalExpression ''
          default:
            lock_screen:
              timeout 300
              command "swaylock"
            end
            suspend:
              timeout 600
              command "systemctl suspend"
            end
          end
        '';
      };

      target = mkOption {
        type = types.nonEmptyStr;
        default = "graphical-session.target";
        description = "The systemd user target after which Stasis is started.";
      };

      extraArgs = mkOption {
        type = types.listOf types.str;
        default = defaultArgs;
        defaultText = literalExpression ''
          if services.stasis.extraConfig != null then
            [ "-c" "/etc/stasis/stasis.rune" ]
          else
            []
        '';
        description = ''
          Extra arguments to pass to Stasis.
          The default arguments from this module will be **ignored** if you override this.
        '';
      };

      environmentFile = mkOption {
        type = types.nullOr types.str;
        default = "%h/.config/stasis/stasis.env";
        description = ''
          Optional environment file read by the Stasis systemd user service.
          Useful for compositor-specific variables like NIRI_SOCKET.
          Set to null to disable.
        '';
      };

      # Optional: let users extend PATH without forking the module.
      extraPathPackages = mkOption {
        type = types.listOf types.package;
        default = [ ];
        example = literalExpression ''with pkgs; [ playerctl swaylock brightnessctl ]'';
        description = ''
          Extra packages added to the Stasis systemd user service PATH.
          (The module already includes `pulseaudio` so `pactl` is available.)
        '';
      };

      tray.enable = mkEnableOption "the optional Stasis StatusNotifier tray frontend";
    };
  };

  config = mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    systemd.user.services.stasis = {
      description = "Stasis Wayland Idle Manager";
      after = [ cfg.target ];
      partOf = [ cfg.target ];
      wantedBy = [ cfg.target ];

      # systemd will construct PATH from these package derivations safely.
      path = baseServicePathPkgs ++ cfg.extraPathPackages;

      serviceConfig = mkMerge [
        {
          Type = "simple";
          ExecStart = "${getExe cfg.package} ${escapeShellArgs cfg.extraArgs}";
          Restart = "on-failure";

          PassEnvironment = [
            "NIRI_SOCKET"
            "WAYLAND_DISPLAY"
            "XDG_RUNTIME_DIR"
            "DBUS_SESSION_BUS_ADDRESS"
          ];
        }
        (mkIf (cfg.environmentFile != null) {
          EnvironmentFile = [ "-${cfg.environmentFile}" ];
        })
      ];
    };

    systemd.user.services.stasis-tray = mkIf cfg.tray.enable {
      description = "Stasis System Tray Frontend";
      after = [ cfg.target "stasis.service" ];
      partOf = [ cfg.target ];
      wantedBy = [ cfg.target ];

      path = baseServicePathPkgs ++ cfg.extraPathPackages;

      serviceConfig = mkMerge [
        {
          Type = "simple";
          ExecStart = "${getExe cfg.package} tray";
          Restart = "on-failure";

          PassEnvironment = [
            "WAYLAND_DISPLAY"
            "XDG_RUNTIME_DIR"
            "DBUS_SESSION_BUS_ADDRESS"
          ];
        }
        (mkIf (cfg.environmentFile != null) {
          EnvironmentFile = [ "-${cfg.environmentFile}" ];
        })
      ];
    };

    environment.etc."stasis/stasis.rune" = mkIf (cfg.extraConfig != null) {
      text = cfg.extraConfig;
    };
  };
}
