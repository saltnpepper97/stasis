with import <nixpkgs> {}; let
  inherit (lib) licenses maintainers platforms;
in
  rustPlatform.buildRustPackage rec {
    pname = "stasis";
    version = "0.2.1";

    src = ./.;

    cargoLock = {
      lockFile = ./Cargo.lock;
    };

    nativeBuildInputs = [
      pkg-config
      wayland-scanner
    ];

    buildInputs = [
      wayland
      wayland-protocols
      dbus
      systemd
      alsa-lib
      pulseaudio
      libinput
      udev
    ];

    # Required environment variables for Wayland protocol generation
    WAYLAND_SCANNER = "${wayland-scanner}/bin/wayland-scanner";

    # Enable Wayland protocols during build
    preBuild = ''
      export PKG_CONFIG_PATH="${wayland-protocols}/share/pkgconfig:$PKG_CONFIG_PATH"
    '';

    # Tests require a Wayland session
    doCheck = false;

    postInstall = ''
      # Install man page
      install -Dm644 man/stasis.5 $out/share/man/man5/stasis.5
    '';

    meta = with lib; {
      description = "A modern Wayland idle manager that knows when to step back";
      longDescription = ''
        Stasis is a smart idle manager for Wayland that understands context.
        It automatically prevents idle when watching videos, reading documents,
        or playing music, while allowing idle when appropriate. Features include
        media-aware idle handling, application-specific inhibitors, Wayland idle
        inhibitor protocol support, and flexible configuration using the RUNE
        configuration language.
      '';
      homepage = "https://github.com/saltnpepper97/stasis";
      license = licenses.mit;
      maintainers = with maintainers; [];
      platforms = platforms.linux;
      mainProgram = "stasis";
    };
  }
