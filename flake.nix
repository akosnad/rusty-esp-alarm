{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    esp-dev = {
      url = "github:mirrexagon/nixpkgs-esp-dev/6c34f2436015eb6c107970d9b88f3d5d4600c6fa";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane/v0.20.3";
    hercules-ci-effects = {
      url = "github:hercules-ci/hercules-ci-effects";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-parts.follows = "flake-parts";
    };
  };

  outputs =
    inputs@{
      self,
      flake-parts,
      fenix,
      crane,
      esp-dev,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } (
      { config, withSystem, ... }:
      {
        imports = [
          inputs.treefmt-nix.flakeModule
          inputs.hercules-ci-effects.flakeModule
          ./nix/flake-module.nix
          ./nix/effects.nix
        ];
        systems = [ "x86_64-linux" ];
        flake.overlays.default = import ./nix/overlays.nix;
        perSystem =
          {
            config,
            pkgs,
            system,
            ...
          }:
          {
            _module.args.pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [
                fenix.overlays.default
                esp-dev.overlays.default
                self.overlays.default
              ];
            };

            project.toolchain = with fenix.packages.${system};
              combine (with pkgs; [
                rust-esp
                rust-src-esp
              ]);

            treefmt = {
              projectRootFile = "Cargo.toml";
              programs = {
                nixfmt.enable = true;
                rustfmt = {
                  enable = true;
                  package = config.project.toolchain;
                };
              };
            };
          };
      }
    );
}
