(() => {
  const op = globalThis?.Deno?.core?.ops?.op_pg_execute
    ?? globalThis?.__pg_op_execute;
  if (typeof op !== "function") return;

  const hasOwn = Object.prototype.hasOwnProperty;

  const encodeType = (value) => {
    if (typeof value === "string") {
      return { kind: "name", value };
    }
    throw new TypeError("_pg.execute typed parameters require a string type name");
  };

  const isPlainObject = (value) => {
    if (value === null || typeof value !== "object") return false;
    const proto = Object.getPrototypeOf(value);
    return proto === Object.prototype || proto === null;
  };

  const isTypedParam = (value) =>
    isPlainObject(value)
    && hasOwn.call(value, "type")
    && hasOwn.call(value, "value")
    && Object.keys(value).length === 2;

  const encodeValue = (value) => {
    if (value === null) return { kind: "null" };
    if (value === undefined) {
      throw new TypeError("_pg.execute does not accept undefined parameters");
    }
    if (typeof value === "boolean") return { kind: "bool", value };
    if (typeof value === "number") return { kind: "number", value };
    if (typeof value === "string") return { kind: "string", value };
    if (typeof value === "bigint") return { kind: "bigint", value: value.toString() };
    if (Array.isArray(value)) {
      return { kind: "array", value: value.map(encodeValue) };
    }
    if (isPlainObject(value)) {
      return {
        kind: "object",
        value: Object.fromEntries(
          Object.entries(value).map(([key, entry]) => [key, encodeValue(entry)]),
        ),
      };
    }
    throw new TypeError(
      `_pg.execute only accepts primitive values, arrays, plain objects, or typed parameter objects; got ${Object.prototype.toString.call(value)}`,
    );
  };

  const encodeParam = (value) => {
    if (isTypedParam(value)) {
      return {
        kind: "typed",
        type: encodeType(value.type),
        value: encodeValue(value.value),
      };
    }
    return {
      kind: "inferred",
      value: encodeValue(value),
    };
  };

  Object.defineProperty(globalThis, "_pg", {
    value: {
      execute(sql, ...params) {
        if (typeof sql !== "string") {
          throw new TypeError("_pg.execute requires the SQL argument to be a string");
        }
        return op(sql, params.map(encodeParam));
      },
    },
    configurable: true,
    enumerable: false,
    writable: true,
  });
})();
