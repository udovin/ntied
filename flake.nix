{
  description = "ntied messenger flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forEachSystem = lib.genAttrs systems;
      mkBaseRuntimeLibs = pkgs:
        with pkgs; [
          alsa-lib
          libpulseaudio
          pipewire
          libxkbcommon
          wayland
          libGL
          mesa
          vulkan-loader
          xorg.libX11
          xorg.libXcursor
          xorg.libXrandr
          xorg.libXi
          xorg.libXinerama
          xorg.libXext
          xorg.libXfixes
          udev
        ];
    in {
      packages = forEachSystem (system:
        let
          overlays = [
            (import rust-overlay)
          ];
          pkgs = import nixpkgs {
            inherit system;
            overlays = overlays;
          };
          mingwPkgs = pkgs.pkgsCross.mingwW64;
          rustToolchain = pkgs.rust-bin.stable.latest.default;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          windowsTarget = "x86_64-pc-windows-gnu";
          windowsRustToolchain = rustToolchain.override {
            targets = [ windowsTarget ];
          };
          windowsRustPlatform = mingwPkgs.makeRustPlatform {
            cargo = windowsRustToolchain;
            rustc = windowsRustToolchain;
          };
          baseRuntimeLibs = mkBaseRuntimeLibs pkgs;
          runtimeLibs = baseRuntimeLibs ++ [ pkgs.openssl ];
          runtimeLibPath = pkgs.lib.makeLibraryPath runtimeLibs;
        in {
          ntied = rustPlatform.buildRustPackage {
            pname = "ntied";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "--workspace" "--bins" ];
            cargoTestFlags = [ "--workspace" "--bins" ];
            nativeBuildInputs = with pkgs; [
              pkg-config
              makeWrapper
            ];
            buildInputs = runtimeLibs;
            postInstall = ''
              wrapProgram $out/bin/ntied \
                --prefix LD_LIBRARY_PATH : ${runtimeLibPath}
            '';
          };

          ntied-windows = windowsRustPlatform.buildRustPackage {
            pname = "ntied";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "--workspace"
              "--bins"
              "--target"
              windowsTarget
            ];
            nativeBuildInputs = with pkgs; [
              pkg-config
            ];
            buildInputs = with mingwPkgs; [
              openssl
            ];
            doCheck = false;
            dontPatchELF = true;
            env = {
              OPENSSL_STATIC = "1";
              PKG_CONFIG_ALLOW_CROSS = "1";
              RUSTFLAGS = "-C target-feature=+crt-static";
            };
          };

          default = self.packages.${system}.ntied;
        });

      devShells = forEachSystem (system:
        let
          overlays = [
            (import rust-overlay)
          ];
          pkgs = import nixpkgs {
            inherit system;
            overlays = overlays;
          };
          baseRuntimeLibs = mkBaseRuntimeLibs pkgs;
          rustToolchain = pkgs.rust-bin.stable.latest.default;
        in {
          default = pkgs.mkShell {
            packages = [
              rustToolchain
              pkgs.rust-analyzer
              pkgs.pkg-config
              pkgs.openssl
              pkgs.cmake
              pkgs.python3
            ] ++ baseRuntimeLibs;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath baseRuntimeLibs;
          };
        });
    };
}
