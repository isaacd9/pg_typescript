# @pg-typescript/types

Ambient TypeScript types for the reserved `pg_typescript` runtime global:

```ts
_pg.execute("select 1 as value");
```

This package provides type information only. It does not install any runtime
code.

## Install

```bash
npm install --save-dev @pg-typescript/types
```

## Use With tsconfig

```json
{
  "compilerOptions": {
    "types": ["@pg-typescript/types"]
  }
}
```

## Use With a Side-Effect Import

```ts
import "@pg-typescript/types";

const result = _pg.execute("select 1 as value");
```

## Example

```ts
type UserRow = {
  id: number;
  name: string;
};

const result = _pg.execute<UserRow>(
  "select id, name from users where id = $1",
  42,
);
```

## Publish

```bash
cd packages/types
npm publish
```

If you do not control the `@pg-typescript` scope, change the package name in
`package.json` before publishing.
