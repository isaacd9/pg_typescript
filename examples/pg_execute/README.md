# _pg.execute Example

This example shows a TypeScript function using `_pg.execute()` twice:

1. Join two tables (`project_members` and `projects`) to find the projects for a user.
2. Use those project IDs to query a third table (`project_notes`).

## Run

Start the demo from the repo root:

```bash
just pg-execute
```

Or run it directly:

```bash
./examples/pg_execute/run.sh
```

That script starts local `pg18`, recreates a fresh `pg_execute_demo`
database, applies `setup.sql`, defines `public.read_two_tables(integer)`, and
prints:

```sql
SELECT jsonb_pretty(public.read_two_tables(1));
```

If you want to load it manually in an existing `psql` session instead:

```sql
\i examples/pg_execute/setup.sql
```

Start `psql` from the repo root for that manual path. `setup.sql` loads the
TypeScript body from [read_two_tables.ts](/Users/isaac/code/pg_deno/examples/pg_execute/read_two_tables.ts)
so the function implementation can live in a real `.ts` file with syntax
highlighting. The file contents are injected by `psql` via `\set ... \`cat ...\``
and then quoted into `CREATE FUNCTION ... AS :'read_two_tables_body'`.

The interesting bit is the TypeScript body:

```ts
type PgParam = null | boolean | number | string | bigint;

type PgResult<Row> = {
  rows: Row[];
};

declare const _pg: {
  execute<Row>(sql: string, ...params: PgParam[]): PgResult<Row>;
};

declare const user_id: number;

type MembershipRow = {
  project_id: number;
  project_name: string;
  role: string;
};

type NoteRow = {
  project_id: number;
  body: string;
};

type ProjectSummary = {
  projectId: number;
  projectName: string;
  role: string;
  notes: string[];
};

const readTwoTables = (user_id: number): ProjectSummary[] => {
  const memberships = _pg.execute<MembershipRow>(
    `
      SELECT
        pm.project_id,
        p.name AS project_name,
        pm.role
      FROM public.project_members AS pm
      JOIN public.projects AS p
        ON p.id = pm.project_id
      WHERE pm.user_id = $1
      ORDER BY pm.project_id
    `,
    user_id,
  ).rows;

  if (memberships.length === 0) {
    return [];
  }

  const projectIds = [...new Set(memberships.map((row) => row.project_id))];
  const placeholders = projectIds.map((_, index) => `$${index + 1}`).join(", ");

  const notes = _pg.execute<NoteRow>(
    `
      SELECT
        project_id,
        body
      FROM public.project_notes
      WHERE project_id IN (${placeholders})
      ORDER BY project_id, id
    `,
    ...projectIds,
  ).rows;

  const notesByProject = new Map<number, string[]>();
  for (const note of notes) {
    const grouped = notesByProject.get(note.project_id) ?? [];
    grouped.push(note.body);
    notesByProject.set(note.project_id, grouped);
  }

  return memberships.map((membership) => ({
    projectId: membership.project_id,
    projectName: membership.project_name,
    role: membership.role,
    notes: notesByProject.get(membership.project_id) ?? [],
  }));
};

// @ts-ignore pg_typescript injects this file as a function body.
return readTwoTables(user_id);
```

Notice that the second query still binds values as parameters. The only string
construction is building the placeholder list (`$1, $2, ...`) from the number
of IDs, not interpolating the IDs directly into SQL.
