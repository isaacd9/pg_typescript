export type PgScalar = null | boolean | number | string;

export type PgJson =
  | PgScalar
  | PgJson[]
  | { [key: string]: PgJson };

export type PgValue = PgJson;

export type PgTypedParam<T = unknown> = {
  type: string;
  value: T;
};

export type PgParam = PgJson | bigint | PgTypedParam;

export type PgRow = Record<string, PgValue>;

export type PgResult<Row = PgRow> = {
  rows: Row[];
  command: string;
  rowCount: number;
};

export interface PgApi {
  execute<Row = PgRow>(sql: string, ...params: PgParam[]): PgResult<Row>;
}

declare global {
  const _pg: PgApi;
}

export {};
