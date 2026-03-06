"""Continuously feed the streaming demo and print the derived summary rows."""

from __future__ import annotations

import argparse
import json
import random
import subprocess
import sys
import time
from pathlib import Path

# Insert one note and ask Postgres to echo the inserted row back as JSON so the
# demo prints exactly what landed in the primary table.
INSERT_SQL = """\
WITH inserted AS (
  INSERT INTO public.stream_notes (author, body, tags)
  VALUES (:'author', :'body', :'tags'::jsonb)
  RETURNING id, author, body, tags, created_at
)
SELECT row_to_json(inserted)::text FROM inserted;
"""

# Fetch the single summary row that the trigger wrote for the inserted note.
DERIVED_SQL = """\
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
WHERE note_id = {note_id}
"""


def load_word_bank(path: Path) -> list[str]:
    """Read the EFF wordlist and keep only the actual word column."""
    words = [
        parts[-1] for line in path.read_text().splitlines() if (parts := line.split())
    ]

    if not words:
        raise SystemExit(f"No words loaded from {path}.")

    return words


def build_random_note(
    word_bank: list[str], min_words: int, max_words: int
) -> tuple[str, str, list[str]]:
    """Create one synthetic note with repeated topic words and random uppercase."""
    topic = random.choice(word_bank)
    total_words = random.randint(min_words, max_words)
    repeat_count = random.randint(2, 4)
    filler_count = max(1, total_words - repeat_count)

    body_words = [topic] * repeat_count
    body_words.extend(random.choice(word_bank) for _ in range(filler_count))
    random.shuffle(body_words)

    # Random uppercase words make the downstream normalization visible.
    uppercase_count = random.randint(1, max(1, len(body_words) // 3))
    for idx in random.sample(range(len(body_words)), uppercase_count):
        body_words[idx] = body_words[idx].upper()

    author = f"bot_{topic}"
    body = " ".join(body_words)
    tags = [topic, random.choice(word_bank), "stream"]

    return author, body, tags


def run_psql(
    db_uri: str,
    sql: str,
    *,
    vars: dict[str, str] | None = None,
    tuples_only: bool = False,
) -> str:
    """Run a small psql command and return its stdout as text."""
    cmd = ["psql", db_uri, "-X", "-v", "ON_ERROR_STOP=1"]
    if tuples_only:
        cmd.extend(["-A", "-t"])
    else:
        cmd.extend(["-P", "pager=off"])
    if vars:
        for key, value in vars.items():
            cmd.extend(["-v", f"{key}={value}"])
    else:
        cmd.extend(["-c", sql])

    completed = subprocess.run(
        cmd,
        input=None if vars is None else sql,
        text=True,
        capture_output=True,
        check=True,
    )

    return completed.stdout.rstrip()


def insert_note(
    db_uri: str, author: str, body: str, tags: list[str]
) -> dict[str, object]:
    """Insert one note and deserialize the returned row JSON."""
    output = run_psql(
        db_uri,
        INSERT_SQL,
        vars={
            "author": author,
            "body": body,
            "tags": json.dumps(tags),
        },
        tuples_only=True,
    )
    if not output:
        raise SystemExit("No row returned from insert.")
    return json.loads(output)


def main() -> int:
    """Drive the demo loop: insert a note, wait briefly, print its summary."""
    parser = argparse.ArgumentParser(
        description="Continuously insert random notes into the streaming demo."
    )
    parser.add_argument("--db-uri", required=True)
    parser.add_argument("--wordlist", required=True)
    parser.add_argument("--interval", type=float, default=5.0)
    parser.add_argument("--min-words", type=int, default=8)
    parser.add_argument("--max-words", type=int, default=18)
    parser.add_argument("--derived-delay-ms", type=int, default=100)
    args = parser.parse_args()

    min_words = min(args.min_words, args.max_words)
    max_words = max(args.min_words, args.max_words)
    derived_delay_s = max(0.0, args.derived_delay_ms / 1000.0)
    word_bank = load_word_bank(Path(args.wordlist))

    try:
        while True:
            # Measure the whole cycle so inserts stay roughly on the requested cadence.
            cycle_started = time.monotonic()
            author, body, tags = build_random_note(word_bank, min_words, max_words)
            row = insert_note(args.db_uri, author, body, tags)

            print("Inserted note:")
            print(json.dumps(row, indent=2))

            if derived_delay_s > 0:
                time.sleep(derived_delay_s)

            print()
            print(f"Triggered rows after {args.derived_delay_ms}ms:")
            print(run_psql(args.db_uri, DERIVED_SQL.format(note_id=int(row["id"]))))
            print()

            # Sleep only for the remainder of the interval after insert/query work.
            elapsed = time.monotonic() - cycle_started
            time.sleep(max(0.0, args.interval - elapsed))
    except KeyboardInterrupt:
        return 0
    except subprocess.CalledProcessError as exc:
        if exc.stderr:
            print(exc.stderr, file=sys.stderr, end="")
        return exc.returncode


if __name__ == "__main__":
    sys.exit(main())
