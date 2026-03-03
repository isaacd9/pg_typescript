-- Validator accepts a syntactically valid function and it is immediately callable
CREATE OR REPLACE FUNCTION ts_identity(x int) RETURNS int
LANGUAGE typescript AS $$
  return x;
$$;

SELECT ts_identity(99);

-- Validator accepts TypeScript-only syntax (type aliases and annotations)
CREATE OR REPLACE FUNCTION ts_types_ok(x int) RETURNS int
LANGUAGE typescript AS $$
  type Pair = { left: number; right: number };
  const pair: Pair = { left: x, right: x + 1 };
  return pair.right;
$$;

SELECT ts_types_ok(41);

-- Validator rejects syntax errors at CREATE FUNCTION time
CREATE OR REPLACE FUNCTION ts_bad_syntax() RETURNS void
LANGUAGE typescript AS $$
  const x = ;
$$;
