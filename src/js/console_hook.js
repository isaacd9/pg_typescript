(() => {
  const op = globalThis?.Deno?.core?.ops?.op_pg_console_log
    ?? globalThis?.__pg_op_console_log;
  if (typeof op !== "function" || typeof globalThis.console === "undefined") return;

  const stringify = (value) => {
    if (typeof value === "string") return value;
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  };

  const bind = (level) => (...args) => {
    const msg = args.map(stringify).join(" ");
    op(level, msg);
  };

  console.debug = bind("debug");
  console.log = bind("log");
  console.info = bind("info");
  console.warn = bind("warn");
  console.error = bind("error");
})();
