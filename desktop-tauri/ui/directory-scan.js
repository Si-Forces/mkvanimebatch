import { createInspectionCoordinator } from "./inspection-coordinator.js";

/**
 * The preflight scan of an input directory: resolves the typed path to its Directory Identity,
 * inspects it through one Inspection Coordinator, and drives the mode's Queue Tracker through
 * the scan. `queueFor(mode)` gives that tracker; `onSettled()` runs whenever a scan has changed
 * the state the page controls depend on.
 *
 * Build one scanner for the app, not one per mode: its Inspection Coordinator is what lets the
 * Remux and Extract views share one inspection of the same directory.
 */
export function createDirectoryScanner({ invoke, queueFor, onSettled }) {
  const inspectDirectory = createInspectionCoordinator((path, options) => invoke("inspect_directory", { path, options }));

  return async function scanInputDirectory(path, mode, { force = false, options } = {}) {
    const tracker = queueFor(mode);
    const request = tracker.beginScanRequest();
    let scanToken = null;
    path = path.trim();
    if (!path) {
      tracker.reset();
      onSettled();
      return false;
    }
    // The UI compares directories byte for byte, so the typed spelling becomes its Directory
    // Identity first. A path the bridge cannot resolve stays as typed, and `inspect_directory`
    // then reports why.
    const directory = await invoke("canonical_directory", { path }).catch(() => path);
    if (!tracker.isNewestScanRequest(request)) return false;
    try {
      if (!force && tracker.reuseInspection(directory)) return true;

      scanToken = tracker.startScan(directory);
      onSettled();
      const manifest = await inspectDirectory(directory, { force, options });
      const acceptedPath = manifest?.canonical_dir || directory;
      return tracker.acceptInspection(scanToken, acceptedPath, manifest);
    } catch (err) {
      console.error("[inspect_directory] failed:", err);
      if (scanToken !== null) tracker.rejectInspection(scanToken);
      return false;
    } finally {
      onSettled();
    }
  };
}
