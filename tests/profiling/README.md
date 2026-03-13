## Profile (Setup vs Execution)

```bash
just profile
```

This runs `tests/profiling/setup_vs_exec.sql`, which:
- Creates a tiny `typescript` add function and a SQL baseline add function.
- Measures one cold first call and 100 warm calls for each.
- Prints summary stats (`avg`, `p50`, `p95`, `min`, `max`) in microseconds.

## Profile (_pg.execute Workload)

```bash
just profile-pg-execute
```

This runs `tests/profiling/pg_execute_workload.sql`, which:
- Creates a local dataset of projects, memberships, and notes.
- Benchmarks a TypeScript function that does two `_pg.execute()` calls and assembles JSON in JS.
- Compares that against an equivalent SQL function that returns the same JSON shape.
- Measures one cold first call and 100 warm calls for each.
