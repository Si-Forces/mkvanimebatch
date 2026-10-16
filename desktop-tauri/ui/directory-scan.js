/**
 * The scan of an input directory: sends the `ScanDirectory` Intent to the Desktop Core, which
 * resolves the typed path to its Directory Identity, reuses or inspects it, and publishes the
 * mode's queue. `syncState()` applies the View State the core published once the scan settled,
 * so a caller that reads the queue after the answer reads that state; `onSettled()` then lets the
 * page controls catch up.
 *
 * Resolves `true` when the queue took the inspection (or reused the one it held), and `false`
 * when a newer scan of the mode overtook it, it failed, or an empty path reset the queue.
 */
export function createDirectoryScanner({ invoke, syncState, onSettled }) {
  return async function scanInputDirectory(path, mode, { force = false, noCache = false } = {}) {
    try {
      const intent = { type: "scan_directory", mode, path, force, no_cache: noCache };
      return (await invoke("dispatch", { intent })) === true;
    } catch (err) {
      console.error("[scan_directory] failed:", err);
      return false;
    } finally {
      await syncState();
      onSettled();
    }
  };
}
