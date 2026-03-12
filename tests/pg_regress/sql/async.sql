-- async/await works: the event loop is drained before the result is returned
CREATE OR REPLACE FUNCTION ts_async_double(n int) RETURNS int
LANGUAGE typescript AS $$
  return await Promise.resolve(n * 2);
$$;

SELECT ts_async_double(21);
SELECT ts_async_double(0);

-- chained awaits
CREATE OR REPLACE FUNCTION ts_async_chain(x int) RETURNS int
LANGUAGE typescript AS $$
  const a = await Promise.resolve(x + 1);
  const b = await Promise.resolve(a * 2);
  return b;
$$;

SELECT ts_async_chain(4);

-- setTimeout callbacks run before the result is returned
CREATE OR REPLACE FUNCTION ts_async_timeout(n int) RETURNS int
LANGUAGE typescript AS $$
  return await new Promise<number>((resolve) => {
    setTimeout(() => resolve(n * 3), 0);
  });
$$;

SELECT ts_async_timeout(7);

-- multiple setTimeout callbacks, including a nested one, are drained in order
CREATE OR REPLACE FUNCTION ts_async_timeouts() RETURNS text
LANGUAGE typescript AS $$
  return await new Promise<string>((resolve) => {
    const seen: string[] = [];

    setTimeout(() => {
      seen.push("first");
    }, 0);

    setTimeout(() => {
      seen.push("second");
      setTimeout(() => {
        seen.push("third");
        resolve(seen.join(","));
      }, 0);
    }, 0);
  });
$$;

SELECT ts_async_timeouts();
