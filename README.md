# pg_typescript

This is a postgres extension, build with `pgrx` in Rust that allows users to run
TypeScript functions in PostgreSQL via Deno/V8.

## Run

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

```bash
cargo pgrx test pg18
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
