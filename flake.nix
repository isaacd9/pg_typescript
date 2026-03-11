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
        aarch64CrossCc = pkgs.pkgsCross.aarch64-multiplatform.stdenv.cc;
        aarch64CrossGcc = "${aarch64CrossCc}/bin/aarch64-unknown-linux-gnu-gcc";
        aarch64CrossLibc = aarch64CrossCc.libc;

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
          llvmPackages.libclang
          # `cargo pgrx init` configures PostgreSQL with ICU enabled by default.
          icu
        ];

        v8BuilderPackages = with pkgs; [
          cargo
          rustc
          python3
          pkg-config
          clang
          binutils
          # Chromium's downloaded clang still needs host libc headers and GCC
          # crt/libgcc objects visible from the FHS /usr sysroot.
          gcc.cc
          llvmPackages.libclang
          llvmPackages.lld
          glibc.dev
          openssl
          zlib
          readline
          flex
          bison
          libxml2
          libxslt
          glib
          glib.dev
          icu
          perl
          ninja
          gnumake
          git
          curl
        ];

        v8BuilderFhs = pkgs.buildFHSEnv {
          name = "pg-deno-v8-builder";
          runScript = "bash";

          targetPkgs = _:
            v8BuilderPackages
            ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isx86_64 [
              pkgs.qemu
              aarch64CrossCc
            ];

          profile =
            ''
              export LIBCLANG_PATH=${pkgs.llvmPackages.libclang.lib}/lib
              export V8_FROM_SOURCE=1
              # Let rusty_v8 download Chromium's pinned clang toolchain instead
              # of forcing the system /usr layout, which may expose a different
              # LLVM major and miss the expected compiler-rt builtins archive.
              export GN_ARGS="is_component_build=false v8_monolithic=true v8_monolithic_for_shared_library=true"
            ''
            + pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isx86_64 ''
              export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=${aarch64CrossGcc}
              export QEMU_LD_PREFIX=/usr/aarch64-linux-gnu
            '';

          extraBuildCommands =
            pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isx86_64 ''
              if [ ! -x "${aarch64CrossGcc}" ]; then
                echo "could not find aarch64 cross gcc at ${aarch64CrossGcc}" >&2
                exit 1
              fi

              if [ ! -d "${aarch64CrossLibc}/lib" ]; then
                echo "missing aarch64 cross libc lib dir at ${aarch64CrossLibc}/lib" >&2
                exit 1
              fi

              if [ ! -d "${aarch64CrossLibc.dev}/include" ]; then
                echo "missing aarch64 cross libc include dir at ${aarch64CrossLibc.dev}/include" >&2
                exit 1
              fi

              mkdir -p "$out/usr/aarch64-linux-gnu"
              ln -sfn ${aarch64CrossLibc}/lib "$out/usr/aarch64-linux-gnu/lib"
              ln -sfn ${aarch64CrossLibc.dev}/include "$out/usr/aarch64-linux-gnu/include"
              ln -sfn /usr/aarch64-linux-gnu/lib/ld-linux-aarch64.so.1 \
                "$out/usr/lib64/ld-linux-aarch64.so.1"
            '';
        };
      in {
        packages = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          v8-builder = v8BuilderFhs;
        };

        devShells =
          {
            default = pkgs.mkShell {
              packages = runtimePackages;

              RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

              shellHook = ''
                if [ "$(uname -s)" = "Darwin" ]; then
                  export RUSTY_V8_MIRROR="https://github.com/denoland/rusty_v8/releases/download"
                fi

                if [ "$(uname -s)" = "Linux" ]; then
                  unset V8_FROM_SOURCE GN_ARGS CLANG_BASE_PATH RUSTC_BOOTSTRAP
                  export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"

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
            v8-builder = v8BuilderFhs.env;
          };
      });
}
