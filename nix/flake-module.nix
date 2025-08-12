{ self, flake-parts-lib, lib, inputs, ... }:
let
  inherit (flake-parts-lib) mkPerSystemOption;
  inherit (lib) mkOption types;

  cargoToml = builtins.fromTOML (builtins.readFile (self + /Cargo.toml));
  inherit (cargoToml.package) name version;

  firmwareCargoToml = builtins.fromTOML (builtins.readFile (self + /firmware/Cargo.toml));
  firmwarePackageName = firmwareCargoToml.package.name;
  firmwareBoard = firmwareCargoToml.package.metadata.board;

  firmwareCargoConfig = builtins.fromTOML (builtins.readFile (self + /firmware/.cargo/config.toml));
  firmwareTarget = firmwareCargoConfig.build.target;
  firmwareTarget' = lib.pipe firmwareTarget [
    lib.toUpper
    (builtins.replaceStrings ["-"] ["_"])
  ];
  buildStd = firmwareCargoConfig.unstable.build-std or [];

in
{
  options.perSystem = mkPerSystemOption ({ config, self', inputs', pkgs, system, ... }: {
    options = {
      project.name = mkOption {
        type = types.str;
        default = name;
      };

      project.version = mkOption {
        type = types.str;
        default = version;
      };

      project.src = lib.mkOption {
        type = lib.types.path;
        description = "Source directory for the project";
        # When filtering sources, we want to allow assets other than .rs files
        # TODO: Don't hardcode these!
        default = lib.cleanSourceWith {
          src = self; # The original, unfiltered source
          filter = path: type:
            (lib.hasSuffix "partitions.csv" path) ||
            (lib.hasSuffix "sdkconfig.defaults" path) ||
            (lib.hasSuffix ".cargo/config.toml" path) ||
            # Default filter from crane (allow .rs files)
            (config.project.craneLib.filterCargoSources path type)
          ;
        };
      };

      project.toolchain = mkOption {
        type = types.package;
        default = pkgs.fenix.complete.withComponents [
          "cargo"
          "rustc"
          "rust-src"

          "clippy"
          "rustfmt"
          "rust-analyzer"
          "llvm-tools-preview"
        ];
      };

      project.craneLib = lib.mkOption {
        type = lib.types.lazyAttrsOf lib.types.raw;
        default = (inputs.crane.mkLib pkgs).overrideToolchain config.project.toolchain;
      };

      project.debugger = mkOption {
        type = types.package;
        default = pkgs.openocd;
      };

      project.package = mkOption {
        type = types.package;
        default = config.project.craneLib.mkCargoDerivation {
          inherit (config.project) src;
          pname = name;
          version = version;

          cargoArtifacts = null;
          cargoVendorDir = config.project.craneLib.vendorMultipleCargoDeps {
            cargoLockList = [
              "${self}/Cargo.lock"
              "${config.project.toolchain}/lib/rustlib/src/rust/library/Cargo.lock"
            ];
          };

          nativeBuildInputs = [
            config.project.toolchain
            config.project.craneLib.removeReferencesToRustToolchainHook
            config.project.craneLib.removeReferencesToVendoredSourcesHook
          ] ++ (with pkgs; [
            esp-idf-esp32-with-clang
            ldproxy
            espflash
          ]);

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (
            with pkgs;
            [
              (esp-idf-esp32-with-clang.passthru.tools.esp-clang)
              zlib
              libxml2
              stdenv.cc.cc.lib
            ]
          );
          LIBCLANG_PATH = "${pkgs.esp-idf-esp32-with-clang.passthru.tools.esp-clang}/esp-clang/lib";

          strictDeps = true;
          doCheck = false;
          dontCheck = true;
          doNotPostBuildInstallCargoBinaries = true;
          doInstallCargoArtifacts = false;

          RUSTFLAGS = lib.concatStringsSep " " firmwareCargoConfig.build.rustflags;
          CARGO_BUILD_TARGET = firmwareTarget;
          "CARGO_TARGET_${firmwareTarget'}_LINKER" = firmwareCargoConfig.target.${firmwareTarget}.linker;
          buildPhaseCargoCommand = "cargo build -vv -p ${firmwarePackageName} --release -Zbuild-std=${lib.concatStringsSep "," buildStd}";
          cargoExtraArgs = "";
          installPhaseCommand = /* sh */ ''
            mkdir -p $out/bin
            cp "target/${firmwareTarget}/release/${firmwarePackageName}" $out/bin/
            cp target/"${firmwareTarget}"/release/{bootloader.bin,partition-table.bin} $out/
            cp firmware/partitions.csv $out/
            espflash save-image --chip ${firmwareBoard} --partition-table firmware/partitions.csv "$out/bin/${firmwarePackageName}" $out/ota.bin
          '';
        };
      };
    };

    config = {
      devShells.default = pkgs.mkShell {
        name = "${name}-shell";
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (
          with pkgs;
          [
            (esp-idf-esp32-with-clang.passthru.tools.esp-clang)
            zlib
            libxml2
            stdenv.cc.cc.lib
          ]
        );
        LIBCLANG_PATH = "${pkgs.esp-idf-esp32-with-clang.passthru.tools.esp-clang}/esp-clang/lib";
        inputsFrom = [
          config.treefmt.build.devShell
          config.project.toolchain
        ];
        packages = [
          config.project.toolchain
          config.project.debugger
          pkgs.espflash
          pkgs.ldproxy
          pkgs.gcc
          pkgs.lld
          pkgs.cargo-binutils
          pkgs.gdb

          pkgs.esp-idf-esp32-with-clang
          pkgs.esp-idf-esp32-with-clang.passthru.tools.esp-clang
          pkgs.cargo-generate
          pkgs.cargo-espflash
        ];
      };

      packages = {
        inherit (config.project) toolchain debugger;
        default = config.project.package;
      };

      checks.package = config.project.package;
    };
  });
}
