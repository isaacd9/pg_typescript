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
        devShells.default = pkgs.mkShell ({
          packages = with pkgs; [
            cargo
            rustc
            cargo-pgrx
            just
            uv
            postgrest
            postgresql_18
            python3
            pkg-config
            clang
            binutils
            llvmPackages.libclang
            llvmPackages.lld
            openssl
            zlib
            readline
            flex
            bison
            libxml2
            libxslt
            glib
            ccache
            icu
            perl
            ninja
            gnumake
            git
            curl
          ];

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          # V8's prebuilt static library uses local-exec TLS, which is
          # incompatible with shared objects (like PG extensions).  Build
          # V8 from source with v8_monolithic_for_shared_library, which
          # switches TLS to local-dynamic model via V8_TLS_USED_IN_LIBRARY.
          V8_FROM_SOURCE = "1";
          GN_ARGS = "v8_monolithic=true v8_monolithic_for_shared_library=true";
          # Use gcc instead of clang for V8 build — avoids needing to
          # assemble a clang+compiler-rt directory tree that matches
          # Chromium's expected layout, and avoids Chromium's clang
          # download (which only ships x86_64 binaries).
          DISABLE_CLANG = "1";
        });
      });
}
