#!/usr/bin/env bash
set -euo pipefail

pg_version="${1:-pg18}"
api_port="${2:-3000}"
db_name="${3:-postgrest_demo}"
postgrest_args=("${@:4}")

for cmd in postgrest psql; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "$cmd not found in PATH." >&2
    exit 1
  fi
done

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"
setup_sql="${script_dir}/setup.sql"
bootstrap_sql="${script_dir}/bootstrap.sql"

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

export PGRST_DB_URI="postgres://postgrest_authenticator:postgrest_dev_password@127.0.0.1:${pg_port}/${db_name}"
export PGRST_DB_SCHEMAS="${PGRST_DB_SCHEMAS:-public}"
export PGRST_DB_ANON_ROLE="${PGRST_DB_ANON_ROLE:-web_anon}"
export PGRST_SERVER_HOST="${PGRST_SERVER_HOST:-127.0.0.1}"
export PGRST_SERVER_PORT="${api_port}"

echo "Starting PostgREST on http://${PGRST_SERVER_HOST}:${PGRST_SERVER_PORT}"
echo "Using schemas: ${PGRST_DB_SCHEMAS}"
echo "Anon role: ${PGRST_DB_ANON_ROLE}"

exec postgrest "${postgrest_args[@]}"
