{
  description = "lumier: a museum-shaped viewer for a photo library";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        lumier = pkgs.callPackage ./nix/package.nix { };
        default = lumier;
      });

      # Builds the package from the importing system's own nixpkgs, so it
      # does not pull in a second copy of nixpkgs.
      nixosModules.default = ./nix/module.nix;

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
