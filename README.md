# pg_typescript

This is a postgres extension, built with `pgrx` in Rust that allows users to
run TypeScript functions in PostgreSQL via Deno/V8.

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

## PostgREST Example

```bash
just postgrest
```

This starts local `pg18`, recreates a fresh `postgrest_demo` database, resets
the demo roles (`web_anon`, `postgrest_authenticator`), applies
`examples/postgrest/setup.sql`, and launches PostgREST on
`http://127.0.0.1:3000`.

See `examples/postgrest/README.md` for curl commands to insert rows and read
auto-generated JSON payloads.

## Streaming Example

```bash
just streaming
```

This starts local `pg18`, recreates a fresh `streaming_demo` database, applies
`examples/streaming/setup.sql`, continuously inserts random notes from the
vendored EFF short wordlist, waits about 100ms after each insert, and prints
the derived summary row written by the trigger.

The trigger calls a TypeScript function that returns a named composite type
(`public.stream_note_summary`) directly, so the demo shows single-row
structured returns rather than a JSONB recordset expansion.

See `examples/streaming/README.md` for the exact shape of the demo output and
the manual SQL behind it.

## Profile (Setup vs Execution)

```bash
just profile
```

This runs `tests/profiling/setup_vs_exec.sql`, which:
- Creates a tiny `typescript` add function and a SQL baseline add function.
- Measures one cold first call and 100 warm calls for each.
- Prints summary stats (`avg`, `p50`, `p95`, `min`, `max`) in microseconds.
