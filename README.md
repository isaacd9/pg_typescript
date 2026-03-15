# pg_typescript
[![CI](https://github.com/isaacd9/pg_typescript/actions/workflows/ci.yml/badge.svg)](https://github.com/isaacd9/pg_typescript/actions/workflows/ci.yml)

A PostgreSQL extension that lets you write functions in TypeScript, powered by
Deno's APIs and the v8 runtime. This borrows some ideas from
[plv8](https://github.com/plv8/plv8), but aims to support TypeScript directly,
as well as a larger set of features, including access to common Node.js APIs.

- Full TypeScript with type annotations and `async`/`await`.
- npm packages can be imported and then are cached in PostgreSQL at `CREATE FUNCTION` time.
- Sandboxed execution, but targeting full Node.js compatibility with [Deno's permission model](https://docs.deno.com/runtime/fundamentals/security/).
- Aims to support the full PostgreSQL type system, including composite types.
  Types like `RECORD`, `JSONB` are mapped automatically between PostgreSQL and
  JavaScript.
- Provides an API for calling back into PostgreSQL from TypeScript (`_pg.execute()`).

```sql
LOAD 'pg_typescript';
CREATE EXTENSION IF NOT EXISTS pg_typescript;

CREATE FUNCTION slugify(title text) RETURNS text
LANGUAGE typescript
SET "typescript.import_map" = '{"imports":{"lodash":"https://esm.sh/lodash@4"}}'
AS $$
  return lodash.kebabCase(title);
$$;

SELECT slugify('Hello World');  -- 'hello-world'
```

## Status

This is **alpha quality** software. Think carefully before using it in
production. The integration tests are comprehensive and in profiling the
performance appears competitive. Eventually, releases will be tagged and
published on GitHub releases. [let me
know](https://github.com/isaacd9/pg_typescript/issues) how it works for you
please!

## Run

Supports PostgreSQL 16, 17, and 18.

### From source

```bash
cargo pgrx run pg18  # or pg17, pg16
```

<details>
<summary>Pre-built Linux packages</summary>

The [CI workflow](https://github.com/isaacd9/pg_typescript/actions/workflows/ci.yml)
builds and uploads packages for Linux x86_64 and aarch64 on every push. Download
the artifact for your PG version and architecture, then extract into your
PostgreSQL installation:

```bash
tar -xzf package-pg18-x86_64.tar.gz
cp -r pg_typescript-*/lib/postgresql/* $(pg_config --pkglibdir)/
cp -r pg_typescript-*/share/postgresql/extension/* $(pg_config --sharedir)/extension/
```

Then in psql: `CREATE EXTENSION pg_typescript;`

</details>

### Preloading

The first time pg_typescript loads in a backend it initializes the V8 runtime,
which can be slow. To avoid the cold-start on the first query, preload the
library by adding it to `shared_preload_libraries` in `postgresql.conf`:

```
# postgresql.conf
shared_preload_libraries = 'pg_typescript'
```

## Test

Postgres extension tests:

```bash
cargo pgrx test pg18
```

Regression tests:
```bash
just regress
```

## Architecture
PostgreSQL uses one process per backend connection. This extension keeps both
the Deno runtime and the `tokio` runtime local to that backend and stores them
in `thread_local` state. When the extension library is loaded in a backend
process, `_PG_init()` prewarms the Deno runtime; subsequent calls in that same
backend reuse it.

When a function is called, we read the function source from PostgreSQL, wrap it
in a synthetic module, and evaluate it inside the shared backend-local runtime.
Imported module source is fetched ahead of time and stored in a PostgreSQL
table, and the runtime loader reads from that store during module evaluation.
A separate per-backend cache stores the compiled default export handle for each
loaded function so repeated calls do not need to reload and reevaluate the
module.

Permissions are managed by PostgreSQL GUCs (configuration variables), and can be
enforced either on the function call level or by a Superuser with a "maximum"
set. These are set on the Deno runtime on each execution.

## Calling into PostgreSQL

The TypeScript runtime is provided with a `_pg` global variable that a module can
use to call into PostgreSQL. This provides a function, `execute`, that can be
used to execute a PostgreSQL query and return the results mapped back into a JavaScript
object.

The types for this can be found in `packages/types`. Execution can be enabled
for a function or `DO` block via `typescript.allow_pg_execute`, subject to the
superuser cap `typescript.max_allow_pg_execute`.


```sql
CREATE FUNCTION active_user_emails() RETURNS jsonb
LANGUAGE typescript
SET "typescript.allow_pg_execute" = 'on'
AS $$
  const result = await _pg.execute(
    "SELECT email FROM users WHERE active = $1", [true]
  );
  return result.rows.map(r => r.email);
$$;
```

The types for `_pg.execute()` can be found in `packages/types`.

## Permissions

By default all capabilities (network, filesystem, env, etc.) are **denied**.
Permissions are granted via GUCs attached with `SET`:

```sql
-- Allow network access to a specific host
CREATE FUNCTION call_api(url text) RETURNS jsonb
LANGUAGE typescript
SET "typescript.allow_net" = 'api.example.com'
AS $$
  const res = await fetch(url);
  return await res.json();
$$;

-- Allow calling back into PostgreSQL
CREATE FUNCTION count_users() RETURNS int
LANGUAGE typescript
SET "typescript.allow_pg_execute" = 'on'
AS $$
  const result = await _pg.execute('SELECT count(*) as n FROM users');
  return result.rows[0].n;
$$;
```

Superusers can set `max_*` GUCs to cap what any function may request. The types
for `_pg.execute()` can be found in `packages/types`.

<details>
<summary>Full GUC reference</summary>

`Userset` GUCs can be applied with `SET` / `SET LOCAL` for a session or
transaction, or attached to a function with `CREATE FUNCTION ... SET`. The
`max_*` GUCs are superuser-only caps that bound what a function or DO block may
request. Permission-list GUCs accept `off|none|deny|false`, `*|all|on|true`, or
a comma-separated allowlist. `typescript.import_map` expects an [import
map JSON](https://deno.land/manual/typescript/import_maps).

Top-level imports in function bodies must be declared in `typescript.import_map`.
Import-map keys are used as JavaScript identifiers in generated `import * as ...`
statements, so they must be valid JS identifiers such as `lodash` or `_internal`,
not arbitrary package specifiers like `my-pkg`. See the `examples/` directory
for examples of imports.

| GUC | Settable By | Default | Purpose |
| --- | --- | --- | --- |
| `typescript.import_map` | Userset | Unset; treated as no import map | Import map JSON used for function imports and `DO` blocks, for example `{"imports":{"lodash":"https://esm.sh/lodash@4.17.23"}}`. |
| `typescript.max_imports` | Superuser (`Suset`) | Unset; treated as allow all | Cap on which `http(s)` URL prefixes may appear in `typescript.import_map`. |
| `typescript.allow_read` | Userset | Unset; treated as deny | Deno read permission. |
| `typescript.allow_write` | Userset | Unset; treated as deny | Deno write permission. |
| `typescript.allow_net` | Userset | Unset; treated as deny | Deno network permission. |
| `typescript.allow_env` | Userset | Unset; treated as deny | Deno environment-variable permission. |
| `typescript.allow_run` | Userset | Unset; treated as deny | Deno subprocess permission. |
| `typescript.allow_ffi` | Userset | Unset; treated as deny | Deno FFI permission. |
| `typescript.allow_sys` | Userset | Unset; treated as deny | Deno system-information permission. |
| `typescript.allow_import` | Userset | `*` (allow all) | Deno import permission for remote module loading. Defaults to allow because import URLs are already declared in `typescript.import_map`. |
| `typescript.allow_pg_execute` | Userset | Unset; treated as off | Access to `_pg.execute()`. |
| `typescript.max_allow_read` | Superuser (`Suset`) | `*` (uncapped) | Maximum allowed `typescript.allow_read`. |
| `typescript.max_allow_write` | Superuser (`Suset`) | `*` (uncapped) | Maximum allowed `typescript.allow_write`. |
| `typescript.max_allow_net` | Superuser (`Suset`) | `*` (uncapped) | Maximum allowed `typescript.allow_net`. |
| `typescript.max_allow_env` | Superuser (`Suset`) | `*` (uncapped) | Maximum allowed `typescript.allow_env`. |
| `typescript.max_allow_run` | Superuser (`Suset`) | `*` (uncapped) | Maximum allowed `typescript.allow_run`. |
| `typescript.max_allow_ffi` | Superuser (`Suset`) | `*` (uncapped) | Maximum allowed `typescript.allow_ffi`. |
| `typescript.max_allow_sys` | Superuser (`Suset`) | `*` (uncapped) | Maximum allowed `typescript.allow_sys`. |
| `typescript.max_allow_import` | Superuser (`Suset`) | `*` (uncapped) | Maximum allowed `typescript.allow_import`. |
| `typescript.max_allow_pg_execute` | Superuser (`Suset`) | `on` (uncapped) | Maximum allowed `_pg.execute()`. |

</details>

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
