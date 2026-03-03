-- function returning SQL NULL
CREATE OR REPLACE FUNCTION ts_null_ret() RETURNS int
LANGUAGE typescript AS $$
  return null;
$$;

SELECT ts_null_ret() IS NULL AS is_null;

-- null argument: without STRICT, Postgres calls the function even when the
-- argument is NULL; nullish-coalescing lets JS handle it explicitly
CREATE OR REPLACE FUNCTION ts_coalesce(x int) RETURNS int
LANGUAGE typescript AS $$
  return x ?? -1;
$$;

SELECT ts_coalesce(NULL);
SELECT ts_coalesce(42);
