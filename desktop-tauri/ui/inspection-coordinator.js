/**
 * Wraps `inspect(path, options)` so concurrent requests for the same path and
 * `noCache` share one in-flight call. `force` always starts a fresh call, which
 * later same-key requests then share.
 */
export function createInspectionCoordinator(inspect) {
  const activeByKey = new Map();

  return async (path, { force = false, options } = {}) => {
    const key = JSON.stringify([path, options?.noCache ?? false]);
    if (!force && activeByKey.has(key)) return activeByKey.get(key);

    const promise = Promise.resolve().then(() => inspect(path, options));
    activeByKey.set(key, promise);
    try {
      return await promise;
    } finally {
      if (activeByKey.get(key) === promise) activeByKey.delete(key);
    }
  };
}
