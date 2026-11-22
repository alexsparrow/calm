{
  description = "calm: a calendar built on a grammar of time";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        calm = pkgs.callPackage ./nix/package.nix { };
        default = calm;
      });

      apps = forAll (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.system}.calm}/bin/calm";
        };
      });

      # `nixpkgs.overlays = [ calm.overlays.default ]`, then `pkgs.calm`.
      overlays.default = final: _: { calm = final.callPackage ./nix/package.nix { }; };
    };
}
