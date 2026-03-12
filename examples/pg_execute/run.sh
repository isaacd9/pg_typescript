#!/usr/bin/env bash
set -euo pipefail

pg_version="${1:-pg18}"
db_name="${2:-pg_execute_demo}"

for cmd in psql; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "$cmd not found in PATH." >&2
    exit 1
  fi
done

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"
setup_sql="${script_dir}/setup.sql"

cd "${repo_root}"
pg_major="${pg_version#pg}"
pg_port="288${pg_major}"
db_admin_user="${USER:-$(id -un)}"
admin_db_uri="postgres://${db_admin_user}@127.0.0.1:${pg_port}/postgres"
setup_db_uri="postgres://${db_admin_user}@127.0.0.1:${pg_port}/${db_name}"

cargo pgrx start "${pg_version}"
cargo pgrx install --features "${pg_version}" --no-default-features
psql "${admin_db_uri}" -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${db_name} WITH (FORCE)" \
  -c "CREATE DATABASE ${db_name}"
echo "Applying ${setup_sql}"
psql "${setup_db_uri}" -v ON_ERROR_STOP=1 -f "${setup_sql}"
