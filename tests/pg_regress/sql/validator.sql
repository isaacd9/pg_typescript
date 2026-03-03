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

-- Validator accepts enums and typed enum values
CREATE OR REPLACE FUNCTION ts_enum_ok(x int) RETURNS int
LANGUAGE typescript AS $$
  enum Step {
    One = 1,
    Two = 2,
  }
  const step: Step = Step.Two;
  return x + step;
$$;

SELECT ts_enum_ok(40);

-- Validator accepts generic functions with constrained type parameters
CREATE OR REPLACE FUNCTION ts_generic_ok(x int) RETURNS int
LANGUAGE typescript AS $$
  function twice<T extends number>(v: T): number {
    return v * 2;
  }
  return twice(x);
$$;

SELECT ts_generic_ok(21);

-- Validator accepts interface declarations and optional properties
CREATE OR REPLACE FUNCTION ts_interface_ok(x int) RETURNS int
LANGUAGE typescript AS $$
  interface Box {
    value: number;
    note?: string;
  }
  const box: Box = { value: x };
  return box.value;
$$;

SELECT ts_interface_ok(42);

-- Validator rejects syntax errors at CREATE FUNCTION time
CREATE OR REPLACE FUNCTION ts_bad_syntax() RETURNS void
LANGUAGE typescript AS $$
  const x = ;
$$;

-- Additional syntax rejection checks (named to make failures obvious)
CREATE OR REPLACE FUNCTION ts_validator_rejects(stmt text) RETURNS bool
LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE stmt;
  RETURN false;
EXCEPTION WHEN others THEN
  RETURN true;
END;
$$;

SELECT test, ok
FROM (
  VALUES
    (
      '01_reject_missing_expr',
      ts_validator_rejects($sql$
        CREATE OR REPLACE FUNCTION ts_bad_missing_expr() RETURNS void
        LANGUAGE typescript AS $fn$
          const x = ;
        $fn$;
      $sql$)
    ),
    (
      '02_reject_malformed_type',
      ts_validator_rejects($sql$
        CREATE OR REPLACE FUNCTION ts_bad_malformed_type() RETURNS void
        LANGUAGE typescript AS $fn$
          type Pair = { left: number right: number };
        $fn$;
      $sql$)
    ),
    (
      '03_reject_unclosed_params',
      ts_validator_rejects($sql$
        CREATE OR REPLACE FUNCTION ts_bad_unclosed_params() RETURNS void
        LANGUAGE typescript AS $fn$
          function f(x: number {
            return x;
          }
        $fn$;
      $sql$)
    )
) AS checks(test, ok)
ORDER BY test;
