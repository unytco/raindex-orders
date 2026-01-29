{
  description = "Raindex Orders - Holo Claims";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    rainix.url = "github:rainlanguage/rainix";
  };

  outputs = { self, rainix, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = rainix.pkgs.${system};
      in rec {
        packages = rainix.packages.${system};
        devShells = rainix.devShells.${system};
      }
    );
}
