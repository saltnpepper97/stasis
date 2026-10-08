{ package }:
{ config, lib, pkgs, ... }:
let
  inherit (lib)
    mkIf
    mkMerge
    mkEnableOption
    mkPackageOption
    mkOption
    types
    escapeShellArgs
    getExe
    makeBinPath
    literalExpression
    ;
  cfg = config.services.stasis;
  # Base packages available in the service PATH.
  # Native PipeWire tools are preferred; pactl supports genuine PulseAudio.
  baseServicePathPkgs = with pkgs; [
    bashInteractive
    coreutils
    systemd
    pipewire
    pulseaudio
  ];
in
{
  options.services.stasis = {
    enable = mkEnableOption "Stasis, a lightweight, feature rich Wayland idle manager written in Rust";
    package = mkPackageOption { stasis = package; } "stasis" { };
    extraConfig = mkOption {
      type = types.nullOr types.lines;
      default = null;
      description = ''
        The literal contents of the Stasis configuration file.
        If set, Home Manager will write this text to
        `~/.config/stasis/stasis.rune`.
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
      default = config.wayland.systemd.target or "graphical-session.target";
      description = "The systemd user target after which Stasis is started.";
    };
    extraArgs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Extra arguments to pass to Stasis.";
    };
    environmentFile = mkOption {
      type = types.nullOr types.str;
      default = "%h/.config/stasis/stasis.env";
      description = ''
        Optional environment file read by the Stasis systemd user service.
        Useful for compositor-specific variables like NIRI_SOCKET.
        Set to null to disable.

        This only provides environment variables to the service. It does not
        add any commands to PATH.
      '';
    };
    extraPathPackages = mkOption {
      type = types.listOf types.package;
      default = [ ];
      example = literalExpression ''with pkgs; [ playerctl swaylock brightnessctl ]'';
      description = ''
        Extra packages added to the Stasis systemd user service PATH.
        (`pipewire` provides `pw-dump`; `pulseaudio` provides native `pactl`.)

        Add packages here only when Stasis itself or commands referenced by your
        Stasis config need to execute them by name. For example, if your config
        uses `swaylock`, `brightnessctl`, or `playerctl`, add those packages
        here. If you run Stasis under niri and your Stasis build shells out to
        the `niri` command, add `pkgs.niri` here too.

        Users do not need to add `swaylock`, `niri`, or any other package unless
        their Stasis setup actually requires those commands.
      '';
    };
    tray.enable = mkEnableOption "the optional Stasis StatusNotifier tray frontend";
  };
  config = mkIf cfg.enable {
    home.packages = [ cfg.package ];
    systemd.user.services.stasis = {
      Unit = {
        Description = "Stasis Wayland Idle Manager";
        PartOf = [ cfg.target ];
        After = [ cfg.target ];
      };
      Service = mkMerge [
        {
          Type = "simple";
          ExecStart = "${getExe cfg.package} ${escapeShellArgs cfg.extraArgs}";
          Restart = "on-failure";
          Slice = "session.slice";
          Environment = [
            "PATH=${makeBinPath (baseServicePathPkgs ++ cfg.extraPathPackages)}"
          ];
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
      Install = {
        WantedBy = [ cfg.target ];
      };
    };
    systemd.user.services.stasis-tray = mkIf cfg.tray.enable {
      Unit = {
        Description = "Stasis System Tray Frontend";
        PartOf = [ cfg.target ];
        After = [ cfg.target "stasis.service" ];
      };
      Service = mkMerge [
        {
          Type = "simple";
          ExecStart = "${getExe cfg.package} tray";
          Restart = "on-failure";
          Slice = "session.slice";
          Environment = [
            "PATH=${makeBinPath (baseServicePathPkgs ++ cfg.extraPathPackages)}"
          ];
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
      Install = {
        WantedBy = [ cfg.target ];
      };
    };
    xdg.configFile."stasis/stasis.rune" = mkIf (cfg.extraConfig != null) {
      text = cfg.extraConfig;
    };
  };
}
