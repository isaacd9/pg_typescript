-- async/await works: the event loop is drained before the result is returned
CREATE FUNCTION ts_async_double(n int) RETURNS int
LANGUAGE typescript AS $$
  return await Promise.resolve(n * 2);
$$;

SELECT ts_async_double(21);
SELECT ts_async_double(0);

-- chained awaits
CREATE FUNCTION ts_async_chain(x int) RETURNS int
LANGUAGE typescript AS $$
  const a = await Promise.resolve(x + 1);
  const b = await Promise.resolve(a * 2);
  return b;
$$;

SELECT ts_async_chain(4);
