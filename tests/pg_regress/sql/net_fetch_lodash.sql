-- enable net permission for sophiebits.com, fetch it, and process content with lodash
SET typescript.max_allow_net = 'sophiebits.com';
SET typescript.max_allow_import = 'esm.sh';

CREATE OR REPLACE FUNCTION ts_fetch_lodash_sophiebits() RETURNS boolean
LANGUAGE typescript
SET typescript.import_map = '{"imports":{"lodash":"https://esm.sh/lodash@4.17.23"}}'
SET typescript.allow_import = 'esm.sh'
SET typescript.allow_net = 'sophiebits.com'
AS $$
  const countWords = async () => {
    const response = await fetch("https://sophiebits.com/");
    const html = await response.text();
    return lodash.words(html).length;
  };

  return (await countWords()) > 0;
$$;

SELECT ts_fetch_lodash_sophiebits() = true AS ok;
RESET typescript.max_allow_net;
RESET typescript.max_allow_import;
