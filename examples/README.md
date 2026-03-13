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

## _pg.execute Example

```bash
just pg-execute
```

See `examples/pg_execute/README.md` for a small example that uses
`_pg.execute()` from a TypeScript function to:

- join two tables
- use the joined rows to drive a second query against a third table
- return the assembled result as JSONB
