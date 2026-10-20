{
  description = "Kargo: Cargo for Kotlin, a build tool and package manager";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        kargo = pkgs.rustPlatform.buildRustPackage {
          pname = "kargo";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          # The tests download compilers and JDKs.
          doCheck = false;
          meta = {
            description = "Cargo for Kotlin: a build tool and package manager";
            homepage = "https://enekocm7.github.io/kargo/";
            license = [ pkgs.lib.licenses.mit pkgs.lib.licenses.asl20 ];
            mainProgram = "kargo";
          };
        };
        default = kargo;
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.system}.kargo}/bin/kargo";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.rustfmt pkgs.clippy ];
        };
      });
    };
}
