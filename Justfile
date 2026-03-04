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

install-tracy:
  cd {{justfile_directory()}}
  cargo pgrx install --features pg18,tracy --no-default-features

# Profile cold/warm call latency. Use tracy="on" to capture a .tracy trace.
# Example: just profile pg18 on 10
profile pg_version="pg18" tracy="off" capture_seconds="10" trace_out="":
  #!/usr/bin/env bash
  set -euo pipefail
  cd "{{justfile_directory()}}"
  cargo pgrx start {{pg_version}}
  features="{{pg_version}}"
  [[ "{{tracy}}" == "on" ]] && features="$features,tracy"
  cargo pgrx install --features "$features" --no-default-features
  if [[ "{{tracy}}" != "on" ]]; then
    cargo pgrx connect {{pg_version}} < tests/profiling/setup_vs_exec.sql
    exit 0
  fi
  command -v tracy-capture >/dev/null || { echo "tracy-capture not found in PATH"; exit 1; }
  trace_file="{{trace_out}}"
  if [[ -z "$trace_file" ]]; then
    mkdir -p traces
    trace_file="traces/profile_$(date +%Y%m%d_%H%M%S).tracy"
  else
    mkdir -p "$(dirname "$trace_file")"
  fi
  echo "Capturing Tracy trace to: $trace_file"
  tracy-capture -f -o "$trace_file" -s {{capture_seconds}} &
  cap_pid=$!
  cargo pgrx connect {{pg_version}} < tests/profiling/setup_vs_exec.sql
  wait "$cap_pid"
  echo "Trace saved: $trace_file"
