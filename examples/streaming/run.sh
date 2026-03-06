#!/usr/bin/env bash
set -euo pipefail

pg_version="${1:-pg18}"
db_name="${2:-streaming_demo}"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"
setup_sql="${script_dir}/setup.sql"
bootstrap_sql="${script_dir}/bootstrap.sql"
producer_py="${script_dir}/producer.py"
wordlist_file="${STREAMING_WORDLIST_FILE:-${script_dir}/eff_short_wordlist_2_0.txt}"
streaming_interval="${STREAMING_INTERVAL_SECONDS:-5}"
streaming_min_words="${STREAMING_MIN_WORDS:-8}"
streaming_max_words="${STREAMING_MAX_WORDS:-18}"
result_delay_ms="${STREAMING_RESULT_DELAY_MS:-100}"

for cmd in psql uv; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "$cmd not found in PATH." >&2
    exit 1
  fi
done

cd "${repo_root}"
pg_major="${pg_version#pg}"
pg_port="288${pg_major}"
db_admin_user="${USER:-$(id -un)}"
admin_db_uri="postgres://${db_admin_user}@127.0.0.1:${pg_port}/postgres"
setup_db_uri="postgres://${db_admin_user}@127.0.0.1:${pg_port}/${db_name}"

cargo pgrx start "${pg_version}"
cargo pgrx install --features "${pg_version}" --no-default-features
cargo pgrx connect "${pg_version}" < "${bootstrap_sql}"
psql "${admin_db_uri}" -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${db_name} WITH (FORCE)" \
  -c "CREATE DATABASE ${db_name}"
echo "Applying ${setup_sql}"
psql "${setup_db_uri}" -v ON_ERROR_STOP=1 -f "${setup_sql}"

echo "Starting streaming demo. Press Ctrl-C to stop."
uv run "${producer_py}" \
  --db-uri "${setup_db_uri}" \
  --wordlist "${wordlist_file}" \
  --interval "${streaming_interval}" \
  --min-words "${streaming_min_words}" \
  --max-words "${streaming_max_words}" \
  --derived-delay-ms "${result_delay_ms}"
