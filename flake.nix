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
          # Keep the Windows payload self-contained by forcing SQLCipher and
          # OpenH264's C++ code onto static MinGW runtime/OpenSSL archives.
          windowsStaticLibs = pkgs.runCommand "ntied-mingw-static-libs" {} ''
            mkdir -p $out/lib
            ln -s ${mingwPkgs.openssl.out}/lib/libcrypto.a $out/lib/libcrypto.a
            ln -s ${mingwPkgs.stdenv.cc.cc}/x86_64-w64-mingw32/lib/libstdc++.a $out/lib/libstdc++.a
            ln -s ${mingwPkgs.stdenv.cc.cc}/lib/gcc/x86_64-w64-mingw32/*/libgcc_eh.a $out/lib/libgcc_eh.a
            ln -s ${mingwPkgs.stdenv.cc.cc}/lib/gcc/x86_64-w64-mingw32/*/libgcc.a $out/lib/libgcc.a
            ln -s ${mingwPkgs.windows.mingw_w64}/lib/libmingwex.a $out/lib/libmingwex.a
            ln -s ${mingwPkgs.windows.mingw_w64}/lib/libmingw32.a $out/lib/libmingw32.a
            ln -s ${mingwPkgs.windows.mcfgthreads}/lib/libmcfgthread.a $out/lib/libmcfgthread.a
            ln -s ${mingwPkgs.windows.pthreads}/lib/libpthread.a $out/lib/libpthread.a
            ln -s ${mingwPkgs.windows.mingw_w64}/lib/libkernel32.a $out/lib/libkernel32.a
            ln -s ${mingwPkgs.windows.mingw_w64}/lib/libntdll.a $out/lib/libntdll.a
          '';
          windowsStaticLinkFlags = pkgs.lib.concatStringsSep " " [
            "-C target-feature=+crt-static"
            "-C link-arg=-static"
            "-C link-arg=-static-libgcc"
            "-C link-arg=-Wl,--start-group"
            "-C link-arg=${windowsStaticLibs}/lib/libstdc++.a"
            "-C link-arg=${windowsStaticLibs}/lib/libgcc_eh.a"
            "-C link-arg=${windowsStaticLibs}/lib/libgcc.a"
            "-C link-arg=${windowsStaticLibs}/lib/libmingwex.a"
            "-C link-arg=${windowsStaticLibs}/lib/libmingw32.a"
            "-C link-arg=${windowsStaticLibs}/lib/libmcfgthread.a"
            "-C link-arg=${windowsStaticLibs}/lib/libpthread.a"
            "-C link-arg=${windowsStaticLibs}/lib/libkernel32.a"
            "-C link-arg=${windowsStaticLibs}/lib/libntdll.a"
            "-C link-arg=-Wl,--end-group"
          ];
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
            buildInputs = with mingwPkgs.windows; [
              pthreads
              mcfgthreads
            ];
            doCheck = false;
            dontPatchELF = true;
            env = {
              OPENSSL_STATIC = "1";
              OPENSSL_LIB_DIR = "${windowsStaticLibs}/lib";
              OPENSSL_INCLUDE_DIR = "${mingwPkgs.openssl.dev}/include";
              PKG_CONFIG_ALLOW_CROSS = "1";
              CXXSTDLIB = "";
              RUSTFLAGS = windowsStaticLinkFlags;
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
