export function createInspectionCoordinator(inspect) {
  const activeByPath = new Map();

  return async (path, { force = false } = {}) => {
    if (!force && activeByPath.has(path)) return activeByPath.get(path);

    const promise = Promise.resolve().then(() => inspect(path));
    activeByPath.set(path, promise);
    try {
      return await promise;
    } finally {
      if (activeByPath.get(path) === promise) activeByPath.delete(path);
    }
  };
}
