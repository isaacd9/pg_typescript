{
  description = "Development shells for pg_deno";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        runtimePackages = with pkgs; [
          cargo
          rustc
          cargo-pgrx
          just
          uv
          postgrest
          postgresql_18
          python3
          pkg-config
          openssl
          zlib
          readline
          flex
          bison
          libxml2
          libxslt
          glib
        ];

        v8BuilderPackages = with pkgs; [
          cargo
          rustc
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
          icu
          perl
          ninja
          gnumake
          git
          curl
        ];
      in {
        devShells =
          {
            default = pkgs.mkShell {
              packages = runtimePackages;

              shellHook = ''
                if [ "$(uname -s)" = "Darwin" ]; then
                  export RUSTY_V8_MIRROR="https://github.com/denoland/rusty_v8/releases/download"
                fi

                if [ "$(uname -s)" = "Linux" ]; then
                  unset V8_FROM_SOURCE GN_ARGS CLANG_BASE_PATH RUSTC_BOOTSTRAP

                  case "$(uname -m)" in
                    x86_64) rusty_v8_target="x86_64-unknown-linux-gnu" ;;
                    aarch64) rusty_v8_target="aarch64-unknown-linux-gnu" ;;
                    *)
                      echo "unsupported Linux architecture for pg_deno V8 prebuilts: $(uname -m)" >&2
                      exit 1
                      ;;
                  esac

                  prebuilt_root="''${PG_DENO_V8_PREBUILT_ROOT:-$PWD/.rusty_v8-prebuilt}"
                  prebuilt_dir="$prebuilt_root/$rusty_v8_target"
                  export RUSTY_V8_ARCHIVE="$prebuilt_dir/librusty_v8.a"
                  export RUSTY_V8_SRC_BINDING_PATH="$prebuilt_dir/src_binding.rs"
                fi
              '';
            };
          } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
            v8-builder = pkgs.mkShell {
              packages = v8BuilderPackages;

              LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
              V8_FROM_SOURCE = "1";
              GN_ARGS = "is_component_build=false v8_monolithic=true v8_monolithic_for_shared_library=true";
            };
          };
      });
}
