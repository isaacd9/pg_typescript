-- functions should work in plain SELECTs and aggregate expressions

CREATE OR REPLACE FUNCTION ts_select_scale(n int) RETURNS int
LANGUAGE typescript AS $$
  const factor = 10;
  return (n ?? 0) * factor;
$$;

CREATE OR REPLACE FUNCTION ts_select_bucket(n int) RETURNS text
LANGUAGE typescript AS $$
  return n % 2 === 0 ? "even" : "odd";
$$;

CREATE OR REPLACE FUNCTION ts_select_json(n int) RETURNS jsonb
LANGUAGE typescript AS $$
  const base = { n };
  return { ...base, sq: n ** 2 };
$$;

-- function in a SELECT list over a set
SELECT count(*) = 3 AND min(scaled) = 10 AND max(scaled) = 30 AS ok
FROM (
  SELECT ts_select_scale(i) AS scaled
  FROM generate_series(1, 3) AS g(i)
) AS s;

-- function in SUM aggregate input
SELECT sum(ts_select_scale(i)) = 100 AS ok
FROM generate_series(1, 4) AS g(i);

-- function in FILTERed aggregate
SELECT sum(ts_select_scale(i)) FILTER (WHERE i > 2) = 70 AS ok
FROM generate_series(1, 4) AS g(i);

-- function in GROUP BY key and aggregate input
WITH agg AS (
  SELECT ts_select_bucket(i) AS bucket, sum(ts_select_scale(i)) AS total
  FROM generate_series(1, 6) AS g(i)
  GROUP BY bucket
)
SELECT bool_and(
  (bucket = 'even' AND total = 120) OR
  (bucket = 'odd' AND total = 90)
) AND count(*) = 2 AS ok
FROM agg;

-- aggregate over JSON output from function
SELECT jsonb_agg(ts_select_json(i) ORDER BY i)
       = '[{"n":1,"sq":1},{"n":2,"sq":4},{"n":3,"sq":9}]'::jsonb AS ok
FROM generate_series(1, 3) AS g(i);
