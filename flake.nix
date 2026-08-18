{
  description = "Kvim terminal editor";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, self }:
    let
      cargoToml = nixpkgs.lib.importTOML ./Cargo.toml;
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShellNoCC {
            packages = [
              pkgs.cargo
              pkgs.clippy
              pkgs.git
              pkgs.nixfmt
              pkgs.ripgrep
              pkgs.rust-analyzer
              pkgs.rustc
              pkgs.rustfmt
            ];
          };
        }
      );

      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          kvim = pkgs.rustPlatform.buildRustPackage {
            pname = "kvim";
            version = cargoToml.workspace.package.version;
            src = nixpkgs.lib.cleanSource self;

            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = [ pkgs.makeWrapper ];

            # The file tree reads the repository state through the `git`
            # command, so its tests need that command inside the build sandbox.
            nativeCheckInputs = [ pkgs.git ];

            postFixup = ''
              wrapProgram "$out/bin/kvim" \
                --prefix PATH : ${
                  nixpkgs.lib.makeBinPath [
                    pkgs.git
                    pkgs.ripgrep
                    pkgs.rust-analyzer
                  ]
                }
            '';

            meta = {
              description = "Modal terminal editor for Rust projects";
              homepage = "https://github.com/patrickjeremic/kvim";
              license = nixpkgs.lib.licenses.mit;
              mainProgram = "kvim";
              platforms = systems;
            };
          };
        in
        {
          default = kvim;
          inherit kvim;
        }
      );

      checks = forAllSystems (system: {
        kvim = self.packages.${system}.kvim;
      });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          meta = self.packages.${system}.kvim.meta;
          program = "${self.packages.${system}.kvim}/bin/kvim";
        };
      });

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
