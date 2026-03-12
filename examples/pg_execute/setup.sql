CREATE EXTENSION IF NOT EXISTS pg_typescript;

DROP FUNCTION IF EXISTS public.read_two_tables(integer);
DROP TABLE IF EXISTS public.project_notes;
DROP TABLE IF EXISTS public.project_members;
DROP TABLE IF EXISTS public.projects;
DROP TABLE IF EXISTS public.app_users;

CREATE TABLE public.app_users (
  id integer PRIMARY KEY,
  name text NOT NULL
);

CREATE TABLE public.projects (
  id integer PRIMARY KEY,
  name text NOT NULL
);

CREATE TABLE public.project_members (
  user_id integer NOT NULL REFERENCES public.app_users(id),
  project_id integer NOT NULL REFERENCES public.projects(id),
  role text NOT NULL,
  PRIMARY KEY (user_id, project_id)
);

CREATE TABLE public.project_notes (
  id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  project_id integer NOT NULL REFERENCES public.projects(id),
  body text NOT NULL
);

INSERT INTO public.app_users (id, name) VALUES
  (1, 'Ada'),
  (2, 'Grace');

INSERT INTO public.projects (id, name) VALUES
  (10, 'Warehouse'),
  (20, 'Billing'),
  (30, 'Search');

INSERT INTO public.project_members (user_id, project_id, role) VALUES
  (1, 10, 'owner'),
  (1, 20, 'reviewer'),
  (2, 30, 'owner');

INSERT INTO public.project_notes (project_id, body) VALUES
  (10, 'Pick/pack flow is live'),
  (10, 'Scanner rollout starts next week'),
  (20, 'Invoice export needs rounding fixes'),
  (30, 'Search synonyms shipped');

-- psql injects the TypeScript here: it reads the file into a variable, then
-- `:'read_two_tables_body'` below expands to a properly quoted SQL string
-- literal for the CREATE FUNCTION body.
\set read_two_tables_body `cat examples/pg_execute/read_two_tables.ts`

SET typescript.max_allow_pg_execute = 'on';

CREATE OR REPLACE FUNCTION public.read_two_tables(user_id integer)
RETURNS jsonb
LANGUAGE typescript
SET typescript.allow_pg_execute = 'on'
AS :'read_two_tables_body';

SELECT jsonb_pretty(public.read_two_tables(1));

RESET typescript.max_allow_pg_execute;
