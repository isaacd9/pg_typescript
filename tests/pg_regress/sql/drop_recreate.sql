SET typescript.max_allow_import = 'esm.sh';

CREATE OR REPLACE FUNCTION ts_drop_recreate_same_name(name text) RETURNS text
LANGUAGE typescript
SET typescript.allow_import = 'esm.sh'
SET typescript.import_map = '{"imports":{"lodash":"https://esm.sh/lodash@4.17.23"}}'
AS $$
  return "v1:" + lodash.capitalize(name);
$$;

SELECT ts_drop_recreate_same_name('hello world') = 'v1:Hello world' AS first_call_ok;

CREATE TEMP TABLE ts_drop_recreate_oids AS
SELECT oid AS old_oid
FROM pg_proc
WHERE proname = 'ts_drop_recreate_same_name'::name
ORDER BY oid DESC
LIMIT 1;

SELECT EXISTS (
  SELECT 1
  FROM deno_internal.deno_package_modules m
  JOIN ts_drop_recreate_oids o ON m.function_oid = o.old_oid
) AS old_cache_populated;

DROP FUNCTION ts_drop_recreate_same_name(text);

SELECT NOT EXISTS (
  SELECT 1
  FROM deno_internal.deno_package_modules m
  JOIN ts_drop_recreate_oids o ON m.function_oid = o.old_oid
) AS old_cache_cleaned;

CREATE OR REPLACE FUNCTION ts_drop_recreate_same_name(name text) RETURNS text
LANGUAGE typescript
SET typescript.allow_import = 'esm.sh'
SET typescript.import_map = '{"imports":{"lodash":"https://esm.sh/lodash@4.17.23"}}'
AS $$
  return "v2:" + lodash.capitalize(name);
$$;

SELECT (SELECT old_oid FROM ts_drop_recreate_oids) <>
       (SELECT oid
        FROM pg_proc
        WHERE proname = 'ts_drop_recreate_same_name'::name
        ORDER BY oid DESC
        LIMIT 1) AS recreated_oid_changed;

SELECT ts_drop_recreate_same_name('hello world') = 'v2:Hello world' AS recreated_call_ok;

DROP TABLE ts_drop_recreate_oids;

RESET typescript.max_allow_import;
