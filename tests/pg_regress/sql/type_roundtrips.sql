-- integer arithmetic
CREATE OR REPLACE FUNCTION ts_add(a int, b int) RETURNS int
LANGUAGE typescript AS $$
  return a + b;
$$;

SELECT ts_add(1, 2);
SELECT ts_add(-5, 10);

-- text
CREATE OR REPLACE FUNCTION ts_greet(name text) RETURNS text
LANGUAGE typescript AS $$
  return `Hello, ${name}!`;
$$;

SELECT ts_greet('world');
SELECT ts_greet('PostgreSQL');

-- boolean
CREATE OR REPLACE FUNCTION ts_gt(a float8, b float8) RETURNS bool
LANGUAGE typescript AS $$
  return a > b;
$$;

SELECT ts_gt(3.0, 1.5);
SELECT ts_gt(1.5, 3.0);

-- float8
CREATE OR REPLACE FUNCTION ts_div(a float8, b float8) RETURNS float8
LANGUAGE typescript AS $$
  return a / b;
$$;

SELECT ts_div(1.0, 4.0);
SELECT ts_div(10.0, 5.0);
