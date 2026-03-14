-- RETURNS RECORD from TypeScript functions

-- 1. OUT parameters (resolved via get_call_result_type)
CREATE OR REPLACE FUNCTION ts_record_out(
  IN x int, OUT a int, OUT b text
) RETURNS RECORD
LANGUAGE typescript AS $$
  return { a: x * 2, b: "hello" };
$$;

SELECT * FROM ts_record_out(21);

-- 2. Caller-side AS clause for anonymous record
CREATE OR REPLACE FUNCTION ts_record_anon(x int) RETURNS RECORD
LANGUAGE typescript AS $$
  return { name: "Alice", age: x };
$$;

SELECT * FROM ts_record_anon(30) AS (name text, age int);

-- 3. NULL return from RECORD function
CREATE OR REPLACE FUNCTION ts_record_null() RETURNS RECORD
LANGUAGE typescript AS $$
  return null;
$$;

SELECT * FROM ts_record_null() AS (a int, b text);

-- 4. Missing fields become NULL
CREATE OR REPLACE FUNCTION ts_record_partial(x int)
  RETURNS RECORD
LANGUAGE typescript AS $$
  return { a: x };
$$;

SELECT * FROM ts_record_partial(42) AS (a int, b text, c bool);

-- 5. Multiple OUT parameters including different types
CREATE OR REPLACE FUNCTION ts_record_multi_out(
  IN val float8,
  OUT doubled float8,
  OUT label text,
  OUT flag bool
) RETURNS RECORD
LANGUAGE typescript AS $$
  return { doubled: val * 2, label: "result", flag: val > 0 };
$$;

SELECT * FROM ts_record_multi_out(3.14);
