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

        # Linux-only helpers for forcing rusty_v8 source builds through the
        # system LLVM toolchain. Keep Darwin shells free of Chromium-specific
        # environment so they continue to use rusty_v8's normal prebuilt flow.
        v8ClangBase =
          if pkgs.stdenv.isLinux then
            let
              clangVer =
                pkgs.lib.versions.major pkgs.llvmPackages.clang-unwrapped.version;
              # Chromium hardcodes a clang_version in build/toolchain/toolchain.gni.
              # Expose our Nix clang payload under the recent Chromium version
              # aliases we've encountered so V8 can find compiler-rt and libc++
              # headers even when the local rusty_v8 fork bumps that constant.
              chromiumClangVers = [ "22" "23" ];
              clangLibTriplet =
                if pkgs.stdenv.hostPlatform.system == "aarch64-linux" then
                  "aarch64-unknown-linux-gnu"
                else if pkgs.stdenv.hostPlatform.system == "x86_64-linux" then
                  "x86_64-unknown-linux-gnu"
                else
                  "";
            in
            pkgs.runCommand "v8-clang-base" {} ''
              mkdir -p $out/bin $out/lib/clang/${clangVer}/lib

              for tool in ${pkgs.llvmPackages.clang-unwrapped}/bin/* \
                          ${pkgs.llvmPackages.llvm}/bin/llvm-* \
                          ${pkgs.llvmPackages.lld}/bin/*; do
                ln -sf "$tool" $out/bin/
              done

              ln -s ${pkgs.llvmPackages.clang}/resource-root/include $out/lib/clang/${clangVer}/include
              ln -s ${pkgs.llvmPackages."compiler-rt-libc"}/share $out/lib/clang/${clangVer}/share
              cp -rL ${pkgs.llvmPackages."compiler-rt-libc"}/lib/. $out/lib/clang/${clangVer}/lib/
              chmod -R u+w $out/lib/clang/${clangVer}/lib

              for archive in $out/lib/clang/${clangVer}/lib/*/libclang_rt.builtins-*.a; do
                dir=$(dirname "$archive")
                ln -sf "$(basename "$archive")" "$dir/libclang_rt.builtins.a"
              done

              if [ -n "${clangLibTriplet}" ] && [ -d "$out/lib/clang/${clangVer}/lib/linux" ]; then
                ln -s linux $out/lib/clang/${clangVer}/lib/${clangLibTriplet}
              fi

              for chromiumClangVer in ${pkgs.lib.concatStringsSep " " chromiumClangVers}; do
                if [ "$chromiumClangVer" != "${clangVer}" ]; then
                  ln -s ${clangVer} $out/lib/clang/$chromiumClangVer
                fi
              done
            ''
          else
            null;

        v8RustBindgenRoot =
          if pkgs.stdenv.isLinux then
            # Chromium's rust GN rules expect a root that contains host-native
            # `bindgen`/`rustfmt` plus `libclang`.
            pkgs.runCommand "v8-rust-bindgen-root" {} ''
              mkdir -p $out/bin $out/lib
              ln -s ${pkgs."rust-bindgen"}/bin/bindgen $out/bin/bindgen
              ln -s ${pkgs.rustfmt}/bin/rustfmt $out/bin/rustfmt
              ln -s ${pkgs.llvmPackages.libclang.lib}/lib/libclang* $out/lib/
            ''
          else
            null;
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
          # We patch v8 to a git checkout so Linux can build from source.
          # In prebuilt mode, rusty_v8 only downloads `src_binding_*.rs` when
          # RUSTY_V8_MIRROR is set; the git checkout does not include those
          # generated bindings under `gen/`, so Darwin needs this to keep using
          # the normal prebuilt release assets.
          RUSTY_V8_MIRROR = "https://github.com/denoland/rusty_v8/releases/download";
          shellHook = pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            # Older shells may still have this exported from a prior dev shell.
            unset DISABLE_CLANG
            export RUSTC_BOOTSTRAP=1

            export GN_ARGS='is_component_build=false v8_monolithic=true v8_monolithic_for_shared_library=true is_clang=true added_rust_stdlib_libs=["adler2"] removed_rust_stdlib_libs=["adler"]'

            if [ "$(uname -m)" = "aarch64" ]; then
              export GN_ARGS="$GN_ARGS rust_sysroot_absolute=\"$(rustc --print sysroot)\" rustc_version=\"$(rustc --version | awk '{print $2}')\" rust_bindgen_root=\"${v8RustBindgenRoot}\""
            fi
          '';
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          # V8's prebuilt static library uses local-exec TLS, which is
          # incompatible with shared objects (like PG extensions).  Build
          # V8 from source with v8_monolithic_for_shared_library, which
          # switches TLS to local-dynamic model via V8_TLS_USED_IN_LIBRARY.
          V8_FROM_SOURCE = "1";
          # Point rusty_v8 at a clang tree assembled from Nix packages.
          # Chromium's clang download only ships x86_64 Linux binaries,
          # so we must use the system clang on aarch64.
          CLANG_BASE_PATH = "${v8ClangBase}";
        });
      });
}
