-- composite type returns from TypeScript functions

CREATE TYPE ts_point AS (x float8, y float8);

-- basic composite return
CREATE OR REPLACE FUNCTION ts_make_point(x float8, y float8) RETURNS ts_point
LANGUAGE typescript AS $$
  return { x, y };
$$;

SELECT (ts_make_point(1.5, 2.5)).*;

-- mixed field types
CREATE TYPE ts_person AS (name text, age int, active bool);

CREATE OR REPLACE FUNCTION ts_make_person(name text, age int) RETURNS ts_person
LANGUAGE typescript AS $$
  return { name, age, active: true };
$$;

SELECT (ts_make_person('Alice', 30)).*;

-- missing fields become NULL
CREATE OR REPLACE FUNCTION ts_partial_point() RETURNS ts_point
LANGUAGE typescript AS $$
  return { x: 42.0 };
$$;

SELECT x, y IS NULL AS y_is_null FROM ts_partial_point();

-- extra JS fields are ignored
CREATE OR REPLACE FUNCTION ts_extra_fields() RETURNS ts_point
LANGUAGE typescript AS $$
  return { x: 1.0, y: 2.0, z: 3.0, label: "ignored" };
$$;

SELECT (ts_extra_fields()).*;

-- null field value
CREATE OR REPLACE FUNCTION ts_null_field() RETURNS ts_person
LANGUAGE typescript AS $$
  return { name: "Bob", age: null, active: false };
$$;

SELECT name, age IS NULL AS age_is_null, active FROM ts_null_field();

-- nested composite type
CREATE TYPE ts_address AS (street text, city text);
CREATE TYPE ts_contact AS (name text, age int, addr ts_address);

CREATE OR REPLACE FUNCTION ts_make_contact() RETURNS ts_contact
LANGUAGE typescript AS $$
  return { name: "Eve", age: 28, addr: { street: "123 Main St", city: "Portland" } };
$$;

SELECT name, age, (addr).street, (addr).city FROM ts_make_contact();
