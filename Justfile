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
