DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'web_anon') THEN
    CREATE ROLE web_anon NOLOGIN;
  ELSE
    ALTER ROLE web_anon NOLOGIN;
  END IF;

  IF NOT EXISTS (
    SELECT 1 FROM pg_roles WHERE rolname = 'postgrest_authenticator'
  ) THEN
    CREATE ROLE postgrest_authenticator LOGIN PASSWORD 'postgrest_dev_password' NOINHERIT;
  ELSE
    ALTER ROLE postgrest_authenticator LOGIN PASSWORD 'postgrest_dev_password' NOINHERIT;
  END IF;
END;
$$;

GRANT web_anon TO postgrest_authenticator;
