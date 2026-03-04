# PostgREST Example

Install PostgREST with:
```
brew install postgrest
```

Make sure you init pgrx on your computer:
```
cargo pgrx init
```

Start PostgREST from the repo root:

```bash
just postgrest
```

In another terminal, you can call the exposed endpoints directly.
`just postgrest` starts from scratch each run by recreating a fresh
`postgrest_demo` database and then applying `setup.sql`.

Insert a row (auto-increment `id`, generated JSON `payload`):

```bash
curl -fsS -X POST 'http://127.0.0.1:3000/postgrest_notes' \
  -H 'content-type: application/json' \
  -H 'prefer: return=representation' \
  -d '{}'
```

Insert another:

```bash
curl -fsS -X POST 'http://127.0.0.1:3000/postgrest_notes' \
  -H 'content-type: application/json' \
  -H 'prefer: return=representation' \
  -d '{}'
```

Read rows:

```bash
curl -fsS 'http://127.0.0.1:3000/postgrest_notes?select=id,payload,created_at&order=id.desc'
```
