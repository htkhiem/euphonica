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
