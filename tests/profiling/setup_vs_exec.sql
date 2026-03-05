\timing on
\set ON_ERROR_STOP on

\echo '=== preload (shared library load) ==='
LOAD 'pg_typescript';

\echo '=== setup ==='
CREATE EXTENSION IF NOT EXISTS pg_typescript;
SET client_min_messages = info;
SET typescript.log_timing = on;

CREATE OR REPLACE FUNCTION ts_profile_add(a integer, b integer) RETURNS integer
LANGUAGE typescript AS $$
  return a + b;
$$;

CREATE OR REPLACE FUNCTION sql_profile_add(a integer, b integer) RETURNS integer
LANGUAGE sql IMMUTABLE AS $$
  SELECT a + b;
$$;

\echo '=== per-call timing (1 cold + 100 warm) ==='
DROP TABLE IF EXISTS pg_typescript_profile_samples;
CREATE UNLOGGED TABLE pg_typescript_profile_samples (
  engine text NOT NULL,
  iter integer NOT NULL,
  elapsed_us double precision NOT NULL,
  PRIMARY KEY (engine, iter)
);

DO $$
DECLARE
  i integer;
  t0 timestamptz;
  t1 timestamptz;
  outv integer;
BEGIN
  t0 := clock_timestamp();
  outv := ts_profile_add(1, 2);
  t1 := clock_timestamp();
  INSERT INTO pg_typescript_profile_samples(engine, iter, elapsed_us)
  VALUES ('typescript', 0, EXTRACT(epoch FROM (t1 - t0)) * 1000000.0);

  FOR i IN 1..100 LOOP
    t0 := clock_timestamp();
    outv := ts_profile_add(i, i + 1);
    t1 := clock_timestamp();
    INSERT INTO pg_typescript_profile_samples(engine, iter, elapsed_us)
    VALUES ('typescript', i, EXTRACT(epoch FROM (t1 - t0)) * 1000000.0);
  END LOOP;
END;
$$;

DO $$
DECLARE
  i integer;
  t0 timestamptz;
  t1 timestamptz;
  outv integer;
BEGIN
  t0 := clock_timestamp();
  outv := sql_profile_add(1, 2);
  t1 := clock_timestamp();
  INSERT INTO pg_typescript_profile_samples(engine, iter, elapsed_us)
  VALUES ('sql', 0, EXTRACT(epoch FROM (t1 - t0)) * 1000000.0);

  FOR i IN 1..100 LOOP
    t0 := clock_timestamp();
    outv := sql_profile_add(i, i + 1);
    t1 := clock_timestamp();
    INSERT INTO pg_typescript_profile_samples(engine, iter, elapsed_us)
    VALUES ('sql', i, EXTRACT(epoch FROM (t1 - t0)) * 1000000.0);
  END LOOP;
END;
$$;

\echo '=== summary (microseconds) ==='
SELECT
  engine,
  round((MAX(elapsed_us) FILTER (WHERE iter = 0))::numeric, 3) AS cold_first_call_us,
  round((AVG(elapsed_us) FILTER (WHERE iter > 0))::numeric, 3) AS warm_avg_us,
  round((percentile_cont(0.50) WITHIN GROUP (ORDER BY elapsed_us)
    FILTER (WHERE iter > 0))::numeric, 3) AS warm_p50_us,
  round((percentile_cont(0.95) WITHIN GROUP (ORDER BY elapsed_us)
    FILTER (WHERE iter > 0))::numeric, 3) AS warm_p95_us,
  round((MIN(elapsed_us) FILTER (WHERE iter > 0))::numeric, 3) AS warm_min_us,
  round((MAX(elapsed_us) FILTER (WHERE iter > 0))::numeric, 3) AS warm_max_us
FROM pg_typescript_profile_samples
GROUP BY engine
ORDER BY engine;

\echo '=== first 10 calls (microseconds) ==='
SELECT engine, iter, round(elapsed_us::numeric, 3) AS us
FROM pg_typescript_profile_samples
ORDER BY engine, iter;

\echo '=== aggregate stats (microseconds) ==='
SELECT
  engine,
  round(AVG(elapsed_us)::numeric, 3)                            AS avg_us,
  round(MIN(elapsed_us)::numeric, 3)                            AS min_us,
  round(MAX(elapsed_us)::numeric, 3)                            AS max_us,
  round(PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY elapsed_us)::numeric, 3) AS median_us,
  round(PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY elapsed_us)::numeric, 3) AS p99_us
FROM pg_typescript_profile_samples
GROUP BY engine
ORDER BY engine;

DROP TABLE pg_typescript_profile_samples;
