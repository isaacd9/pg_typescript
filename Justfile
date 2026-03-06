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

# Profile cold/warm call latency.
# Example: just profile pg18
profile pg_version="pg18":
  #!/usr/bin/env bash
  set -euo pipefail
  cd "{{justfile_directory()}}"
  cargo pgrx start {{pg_version}}
  cargo pgrx install --features "{{pg_version}}" --no-default-features --release
  cargo pgrx connect {{pg_version}} < tests/profiling/setup_vs_exec.sql

# Start PostgREST against the local pgrx Postgres instance.
# Example: just postgrest
postgrest pg_version="pg18" api_port="3000" db_name="postgrest_demo":
  cd "{{justfile_directory()}}"
  ./examples/postgrest/run.sh "{{pg_version}}" "{{api_port}}" "{{db_name}}"

# Set up the streaming demo and continuously print inserted rows plus trigger output.
# Example: just streaming
streaming pg_version="pg18" db_name="streaming_demo":
  cd "{{justfile_directory()}}"
  ./examples/streaming/run.sh "{{pg_version}}" "{{db_name}}"
