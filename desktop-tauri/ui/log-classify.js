// Maps Log Channel lines ({kind, text}) to console CSS classes and to plain text.
// Cosmetic only: no behavior keys off these classes.
// Also holds the DOM-free queue cap the console uses while it waits for a frame.

// One console class per `LogKind`; a `note` is unstyled.
const KIND_CLASS = {
  error: "line-err",
  warning: "line-warn",
  skipped: "line-skip",
  succeeded: "line-ok",
  step: "line-info",
  cache: "line-done",
  note: "",
};

export function lineClass(kind) {
  return KIND_CLASS[kind] ?? "";
}

/** A line as plain text, as mkv-core's `LogLine` renders it: `[ERR] ` before an error's text. */
export function renderLine({ kind, text }) {
  return kind === "error" ? `[ERR] ${text}` : text;
}

/**
 * Returns `queue` with `lines` (one line or a batch) appended, keeping only the
 * newest `max` lines; a `max` of 0 or less keeps none. `queue` itself is left
 * unchanged.
 */
export function pushCapped(queue, lines, max) {
  return max > 0 ? queue.concat(lines).slice(-max) : [];
}
