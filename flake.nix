{
  description = "Development shell for pg_deno";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            cargo-pgrx
            just
            uv
            python3
            pkg-config
            clang
            llvmPackages.libclang
            openssl
            zlib
            readline
            flex
            bison
            libxml2
            libxslt
            ccache
            icu
            perl
            gnumake
            git
            curl
          ];

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        };
      });
}
