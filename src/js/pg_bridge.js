const op = globalThis?.Deno?.core?.ops?.op_pg_execute;
if (typeof op === "function") {
  globalThis.__pg_op_execute = op;
}
