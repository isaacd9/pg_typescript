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
