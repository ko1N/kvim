{
  description = "kvim terminal editor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      self,
    }:
    let
      cargoToml = nixpkgs.lib.importTOML ./Cargo.toml;
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      # `legacyPackages` carries no overlay. Every output imports `nixpkgs`
      # through this function, so `rust-bin` stays available everywhere.
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

      # The development and release toolchain lives in rust-toolchain.toml.
      # Cargo.toml separately owns the minimum supported Rust version.
      toolchainFor = pkgs: pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      msrvToolchainFor = pkgs: pkgs.rust-bin.stable.${cargoToml.workspace.package.rust-version}.minimal;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          msrvToolchain = msrvToolchainFor pkgs;
          rustToolchain = toolchainFor pkgs;
          languageTools = [
            # Language servers. The Rust toolchain supplies `rust-analyzer`.
            pkgs.asm-lsp
            pkgs.bash-language-server
            pkgs.clang-tools
            pkgs.fish-lsp
            pkgs.glsl_analyzer
            pkgs.gopls
            pkgs.lemminx
            pkgs.lua-language-server
            pkgs.marksman
            pkgs.nil
            pkgs.pyright
            pkgs.sqls
            pkgs.taplo
            pkgs.tofu-ls
            pkgs.typescript-language-server
            pkgs.vscode-langservers-extracted
            pkgs.yaml-language-server
            pkgs.zls

            # External formatters. Some packages also supply a server above.
            pkgs.black
            pkgs.gotools
            pkgs.luaformatter
            pkgs.opentofu
            pkgs.prettier
            pkgs.shfmt
            pkgs.sql-formatter
            pkgs.xmlformat
            pkgs.yamlfmt
          ];
        in
        {
          default = pkgs.mkShellNoCC {
            packages = [
              pkgs.git
              pkgs.nixfmt
              pkgs.ripgrep
              # The toolchain supplies Cargo, Rust, rustfmt, Clippy, and
              # `rust-analyzer` at the pinned version.
              rustToolchain
            ]
            ++ languageTools;
          };

          msrv = pkgs.mkShellNoCC {
            packages = [
              pkgs.git
              msrvToolchain
            ];
          };
        }
      );

      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          rustToolchain = toolchainFor pkgs;

          # `pkgs.rustPlatform` builds with the Rust of nixpkgs. This platform
          # builds with the pinned toolchain instead.
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };

          # The wrapper needs one directory that holds `rust-analyzer` alone.
          # The whole toolchain would also put its Cargo and its Rust in front
          # of the commands that the user chose for the edited project.
          rustAnalyzer = pkgs.runCommand "kvim-rust-analyzer" { } ''
            mkdir -p "$out/bin"
            ln -s "${rustToolchain}/bin/rust-analyzer" "$out/bin/rust-analyzer"
          '';

          kvim = rustPlatform.buildRustPackage {
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
                    rustAnalyzer
                  ]
                }
            '';

            meta = {
              description = "Modal terminal editor for Rust projects";
              homepage = "https://github.com/ko1N/kvim";
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

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}
