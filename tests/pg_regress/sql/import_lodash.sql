-- very simple import-map test with lodash from esm.sh
CREATE OR REPLACE FUNCTION ts_lodash_capitalize(name text) RETURNS text
LANGUAGE typescript
SET typescript.import_map = '{"imports":{"lodash":"https://esm.sh/lodash@4.17.23"}}'
AS $$
  return lodash.capitalize(name);
$$;

SELECT ts_lodash_capitalize('hello world') = 'Hello world' AS ok;
SELECT ts_lodash_capitalize('POSTGRES') = 'Postgres' AS ok;

-- lodash chaining with multiple methods
CREATE OR REPLACE FUNCTION ts_lodash_chain(input text) RETURNS jsonb
LANGUAGE typescript
SET typescript.import_map = '{"imports":{"lodash":"https://esm.sh/lodash@4.17.23"}}'
AS $$
  const result = lodash
    .chain(lodash.words(lodash.defaultTo(input, ""), /[A-Za-z']+/g))
    .map((w: string) => lodash.toLower(lodash.trim(w, "'")))
    .filter((w: string) => w.length >= 2)
    .compact()
    .countBy()
    .toPairs()
    .orderBy(["1", "0"], ["desc", "asc"])
    .take(3)
    .map(([word, count]: [string, number]) => ({ word, count }))
    .value();
  return result;
$$;

SELECT ts_lodash_chain('HELLO hello World hello world');
SELECT ts_lodash_chain('') = '[]'::jsonb AS empty_ok;
SELECT ts_lodash_chain(NULL) = '[]'::jsonb AS null_ok;
