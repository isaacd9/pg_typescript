# pg_typescript
[![CI](https://github.com/isaacd9/pg_typescript/actions/workflows/ci.yml/badge.svg)](https://github.com/isaacd9/pg_typescript/actions/workflows/ci.yml)

This is a postgres extension, built with `pgrx` in Rust that allows users to
run TypeScript functions in PostgreSQL via Deno/V8.

This project borrows some ideas from [plv8](https://github.com/plv8/plv8), aim
to support TypeScript, as well as a larger set of features, including access to
common Node.js APIs. This project leverages the Deno permissions model to
sandbox function execution, and only allow the features that a user or
administrator has explicitly granted. Both regular and `async` functions are
supported, and types in PostgreSQL are mapped to TypeScript types.

Imports are resolved via Deno's module resolution mechanism, but must be
imported from a URL, like esm.sh or a GitHub raw URL. When a function is
created, these imports are cached inside a PostgreSQL table, and subsequent
calls to the function will use the cached imports rather than resolving them
again.

## Project Status
This is *alpha* software that you probably shouldn't use in production yet. That
said, the integration tests are comprehensive and the basic functionality works
end to end and appears to have attractive performance.

Eventually, releases will be tagged and published on GitHub releases.

## Architecture
This extension creates a Deno runtime on `_PG_init()` backend, and then reuses
that runtime for subsequent calls. The effect of this, and the Postgres
one-process-per-backend model, is that the runtime (and `tokio` runtime) is
local to a given backend and can be `thread_local`.

When a function is called, we read source code, from the PostgreSQL invocation,
and then inject it into the Deno runtime. The source code is evaluated in the
context of the Deno runtime, and loads modules on-demand from PostgreSQL via
the `pg` module. A per-backend cache stores the compiled module source code,
so that subsequent calls to the same function do not need to recompile it.

Permissions are managed by PostgreSQL GUCs (configuration variables), and can be
enforced either on the function call level or by a Superuser with a "maximum"
set. These are set on the Deno runtime on each execution.

## Calling into PostgreSQL
The TypeScript runtime is provided with a `_pg` global variable that a module can
use to call into PostgreSQL. This provides a function, `execute`, that can be
used to execute a PostgreSQL query and return the results mapped back into a JavaScript
object.

The types for this can be found in `packages/types`. Execution can be enabled or disabled
globally via the `typescript.import_map` GUC.

## GUC Configuration

`Userset` GUCs can be applied with `SET` / `SET LOCAL` for a session or
transaction, or attached to a function with `CREATE FUNCTION ... SET`. The
`max_*` GUCs are superuser-only caps that bound what a function or DO block may
request. Permission-list GUCs accept `off|none|deny|false`, `*|all|on|true`, or
a comma-separated allowlist. `typescript.import_map` expects an [import
map JSON](https://deno.land/manual/typescript/import_maps).

The keys in the import map JSON are module specifiers are used as the module
name in the JavaScript code, and so they must be valid module specifiers. See
the `examples/` directory for examples of imports.

| GUC | Settable By | Default | Purpose |
| --- | --- | --- | --- |
| `typescript.import_map` | Userset | Unset; treated as no import map | Import map JSON used for function imports and `DO` blocks, for example `{"imports":{"lodash":"https://esm.sh/lodash@4.17.23"}}`. |
| `typescript.max_imports` | Superuser (`Suset`) | Unset; treated as allow all | Cap on which `http(s)` URL prefixes may appear in `typescript.import_map`. |
| `typescript.allow_read` | Userset | Unset; treated as deny | Requested Deno read permission for the current function or `DO` block. |
| `typescript.allow_write` | Userset | Unset; treated as deny | Requested Deno write permission for the current function or `DO` block. |
| `typescript.allow_net` | Userset | Unset; treated as deny | Requested Deno network permission for the current function or `DO` block. |
| `typescript.allow_env` | Userset | Unset; treated as deny | Requested Deno environment-variable permission for the current function or `DO` block. |
| `typescript.allow_run` | Userset | Unset; treated as deny | Requested Deno subprocess permission for the current function or `DO` block. |
| `typescript.allow_ffi` | Userset | Unset; treated as deny | Requested Deno FFI permission for the current function or `DO` block. |
| `typescript.allow_sys` | Userset | Unset; treated as deny | Requested Deno system-information permission for the current function or `DO` block. |
| `typescript.allow_import` | Userset | Unset; treated as deny | Requested Deno import permission for remote module loading. |
| `typescript.allow_pg_execute` | Userset | Unset; treated as off | Requested access to `_pg.execute()` for the current function or `DO` block. |
| `typescript.max_allow_read` | Superuser (`Suset`) | Unset; treated as deny | Maximum allowed `typescript.allow_read` request. |
| `typescript.max_allow_write` | Superuser (`Suset`) | Unset; treated as deny | Maximum allowed `typescript.allow_write` request. |
| `typescript.max_allow_net` | Superuser (`Suset`) | Unset; treated as deny | Maximum allowed `typescript.allow_net` request. |
| `typescript.max_allow_env` | Superuser (`Suset`) | Unset; treated as deny | Maximum allowed `typescript.allow_env` request. |
| `typescript.max_allow_run` | Superuser (`Suset`) | Unset; treated as deny | Maximum allowed `typescript.allow_run` request. |
| `typescript.max_allow_ffi` | Superuser (`Suset`) | Unset; treated as deny | Maximum allowed `typescript.allow_ffi` request. |
| `typescript.max_allow_sys` | Superuser (`Suset`) | Unset; treated as deny | Maximum allowed `typescript.allow_sys` request. |
| `typescript.max_allow_import` | Superuser (`Suset`) | Unset; treated as deny | Maximum allowed `typescript.allow_import` request. |
| `typescript.max_allow_pg_execute` | Superuser (`Suset`) | Unset; treated as off | Maximum allowed `_pg.execute()` request. |

## Build

All commands below assume you're running inside the dev shell:

```bash
$ nix develop
```

You can also set up [`direnv`](https://direnv.net/) to automatically load the
development environment when you enter the repo.

### macOS

macOS uses the default upstream `rusty_v8` prebuilt.

```bash
$ cargo build
```

### Linux

Linux needs a custom `rusty_v8` prebuilt built with
`v8_monolithic_for_shared_library=true`, because the stock upstream prebuilt
does not link into a Postgres extension shared library. Both x86_64 and aarch64
targets are supported, but they're cross-compiled onx86_64 Linux.

There's a github workflow in this repository that produces the prebuilt
artifact. You can download it from the latest successful run with `gh run
download`. Place it under `.rusty_v8-prebuilt/`. 

```bash
$ gh run download <run-id> -n rusty-v8-x86_64-unknown-linux-gnu
$ gh run download <run-id> -n rusty-v8-aarch64-unknown-linux-gnu

$ mkdir -p .rusty_v8-prebuilt
$ mv rusty-v8-x86_64-unknown-linux-gnu .rusty_v8-prebuilt/x86_64-unknown-linux-gnu
$ mv rusty-v8-aarch64-unknown-linux-gnu .rusty_v8-prebuilt/aarch64-unknown-linux-gnu
```

If you replace an existing prebuilt in place, run `cargo clean` first so Cargo
does not keep linking an older copied archive from `target/`.

```bash
$ cargo clean
$ cargo build
```

## Run

With Postgres 18:
```bash
cargo pgrx run pg18
```

```sql
CREATE EXTENSION pg_typescript;

CREATE FUNCTION add(a int, b int) RETURNS int LANGUAGE typescript AS $$
  return a + b;
$$;

SELECT add(1, 2);
```

## Test

Unit tests:

```bash
cargo pgrx test pg18
```

Regression tests:
```bash
just regress
```
