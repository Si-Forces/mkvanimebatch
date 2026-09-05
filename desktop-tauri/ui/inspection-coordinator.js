export function createInspectionCoordinator(inspect) {
  const activeByKey = new Map();

  return async (path, { force = false, options } = {}) => {
    const key = JSON.stringify([path, options?.workers ?? null, options?.ioJobs ?? null, options?.noCache ?? false]);
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
