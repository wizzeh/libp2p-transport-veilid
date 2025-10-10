{
    description = "LibP2P Transport Veilid development environment";

    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
        flake-utils.url = "github:numtide/flake-utils";
        rust-overlay.url = "github:oxalica/rust-overlay";
    };

    outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem(system:
        let
            overlays = [ (import rust-overlay) ];
            pkgs = import nixpkgs { inherit system overlays; };
            toolchain = pkgs.rust-bin.selectLatestNightlyWith(toolchain:
                toolchain.default.override {
                    extensions = [
                        "rust-src"
                        "clippy"
                        "rustfmt"
                        "rust-analyzer"
                    ];
                }
            );
        in
        with pkgs;
        {
            devShells.default = mkShell {
                buildInputs = [
                    toolchain
                    cargo-edit
                    cargo-watch
                    pkgs.cmake
                ];
            };
        }
    );
}
