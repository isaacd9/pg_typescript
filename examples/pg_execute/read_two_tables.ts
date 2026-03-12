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
