-- runtime return type mismatches should fail at function execution time
CREATE OR REPLACE FUNCTION ts_assert_raises(stmt text) RETURNS bool
LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE stmt;
  RETURN false;
EXCEPTION WHEN others THEN
  RETURN true;
END;
$$;

CREATE OR REPLACE FUNCTION ts_rt_bool_to_int() RETURNS int
LANGUAGE typescript AS $$
  return true;
$$;

CREATE OR REPLACE FUNCTION ts_rt_number_to_bool() RETURNS bool
LANGUAGE typescript AS $$
  return 1;
$$;

SELECT ts_assert_raises('SELECT ts_rt_bool_to_int()') AS bool_to_int_raises;
SELECT ts_assert_raises('SELECT ts_rt_number_to_bool()') AS number_to_bool_raises;
