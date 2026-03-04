const op = globalThis?.Deno?.core?.ops?.op_pg_console_log;
if (typeof op === "function") {
  globalThis.__pg_op_console_log = op;
}
