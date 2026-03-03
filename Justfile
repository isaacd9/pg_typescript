set shell := ["bash", "-euo", "pipefail", "-c"]

# Run pg_regress and then diff every expected output against results.
regress pg_version="pg18":
  cd {{justfile_directory()}}
  cargo pgrx regress {{pg_version}}
  for expected in tests/pg_regress/expected/*.out; do \
    file="$(basename "$expected")"; \
    diff -u "$expected" "tests/pg_regress/results/$file"; \
  done

test:
  cd {{justfile_directory()}}
  cargo test -v

# Run a manual profiling script to compare cold call vs warm call latency.
profile pg_version="pg18":
  cd {{justfile_directory()}}
  cargo pgrx start {{pg_version}}
  cargo pgrx install --features {{pg_version}} --no-default-features
  cargo pgrx connect {{pg_version}} < tests/profiling/setup_vs_exec.sql
