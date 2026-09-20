{
  description = "Euphonica";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      eachLinuxSystem = nixpkgs.lib.genAttrs [
        "aarch64-linux"
        "x86_64-linux"
      ];
      eachDarwinSystem = nixpkgs.lib.genAttrs [ "aarch64-darwin" ];
    in
    {
      packages = eachLinuxSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          euphonica = pkgs.euphonica.overrideAttrs (_: {
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
            src = self;
            cargoDeps = pkgs.rustPlatform.importCargoLock {
              lockFile = ./Cargo.lock;
              allowBuiltinFetchGit = true;
            };
          });
        in
        {
          inherit euphonica;
          default = euphonica;
        }
      );

      devShells =

        eachLinuxSystem (
          system:
          let
            pkgs = import nixpkgs { inherit system; };
          in
          {
            default = pkgs.mkShell {
              inputsFrom = [ self.packages.${system}.default ];

              GDK_PIXBUF_MODULE_FILE = pkgs.gnome._gdkPixbufCacheBuilder_DO_NOT_USE {
                extraLoaders = [
                  pkgs.librsvg
                  pkgs.webp-pixbuf-loader
                ];
              };

              shellHook = ''
                export XDG_DATA_DIRS="$PWD/build/install/share''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
              '';
            };
          }
        )
        // eachDarwinSystem (
          system:
          let
            pkgs = import nixpkgs { inherit system; };
          in
          {
            default = pkgs.mkShell {
              packages = with pkgs; [
                cargo
                meson
                pkg-config
                rustc
              ];
            };
          }
        );
    };
}
