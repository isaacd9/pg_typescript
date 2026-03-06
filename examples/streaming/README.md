# Streaming Example

Make sure you init pgrx on your computer:
```bash
cargo pgrx init
```

Start the demo from the repo root:

```bash
just streaming
```

This is the world's worst stream processor: `just streaming` continuously
inserts random notes from the committed wordlist every 5 seconds, an
`AFTER INSERT` trigger calls a TypeScript function, and that function uses
lodash to project each note into a single summary row.

`just streaming` starts local `pg18`, recreates a fresh `streaming_demo`
database, applies `setup.sql`, uses the committed
`examples/streaming/eff_short_wordlist_2_0.txt` wordlist, prints each inserted
note, waits about 100ms, and then prints the derived summary row for that note.

The interesting bit is that the TypeScript function returns a real Postgres
composite type:

```sql
CREATE TYPE public.stream_note_summary AS (
  note_id bigint,
  author text,
  normalized_body text,
  total_tokens integer,
  unique_tokens integer,
  dominant_token text,
  dominant_count integer,
  uppercase_token_count integer,
  tag_match_count integer,
  top_tokens jsonb
);
```

That means the trigger can insert the result directly with:

```sql
SELECT (public.ts_project_note_summary(NEW.id, NEW.author, NEW.body, NEW.tags)).*;
```

The generated note bodies randomly uppercase some words on purpose, so
`normalized_body` and the token summary make the normalization step visible.

Stop it with `Ctrl-C`.

Equivalent manual query for the derived summary row:

```sql
SELECT
  note_id,
  author,
  total_tokens,
  unique_tokens,
  dominant_token,
  dominant_count,
  uppercase_token_count,
  tag_match_count,
  normalized_body,
  top_tokens
FROM public.stream_note_summaries
WHERE note_id = <inserted id>;
```

Change the cadence:

```bash
STREAMING_INTERVAL_SECONDS=2 just streaming
```

Manual insert:

```sql
INSERT INTO public.stream_notes (author, body, tags)
VALUES (
  'Ada',
  'POSTGRES postgres TYPESCRIPT abrade academy absurd accountancy',
  '["postgres", "typescript", "demo"]'::jsonb
)
RETURNING id, author, body, tags, created_at;
```

Classic worst-possible stream processor rollup:

```sql
SELECT dominant_token, count(*) AS note_count
FROM public.stream_note_summaries
GROUP BY dominant_token
ORDER BY note_count DESC, dominant_token;
```
