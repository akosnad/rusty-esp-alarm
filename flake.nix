{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem
      (system:
        let
          pkgs = import nixpkgs {
            inherit system;
          };

          fhs = pkgs.buildFHSUserEnv {
            name = "fhs-shell";
            targetPkgs = pkgs: with pkgs; [
              gcc

              pkg-config
              libclang.lib
              gnumake
              cmake
              ninja

              git
              wget

              rustup
              cargo-generate
              espup
              ldproxy

              espflash
              python3
              python3Packages.pip
              python3Packages.virtualenv

              rust-analyzer
            ];
          };
        in
        {
          devShells.default = fhs.env;
          formatter = pkgs.nixpkgs-fmt;
        }
      );
}
