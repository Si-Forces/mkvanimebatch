// The console of each mode: renders the Run Log the Desktop Core keeps (`crates/mkv-desktop-core/src/log.rs`).
// The core decides what a console holds, its error count and its display text; this only draws the
// `job-log` batches and the `get_log` answer it is sent, and keeps their raw text for Copy.

// One console class per `LogKind`; a `note` is unstyled. Cosmetic only: no behavior keys off these classes.
export const KIND_CLASS = {
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

const EMPTY_PROMPT = "No output yet. Run a batch to see its output here.";

/**
 * The consoles of `modes`, each the element `${mode}-log`. `apply(batch)` draws a `job-log` batch in
 * the console of `batch.mode`; `load(mode, view)` takes the `get_log` answer, before which the
 * mode's batches wait, so a batch the answer already covers is not drawn twice. `text(mode)` and
 * `errors(mode)` are what Copy and the Log badge read; `onErrors` runs when a count may have changed.
 */
export function createRunLogs({
  doc,
  modes = ["remux", "extract"],
  onErrors = () => {},
  requestFrame = callback => requestAnimationFrame(callback),
  cancelFrame = id => cancelAnimationFrame(id),
}) {
  const consoles = new Map();
  for (const mode of modes) {
    const con = {
      el: doc.getElementById(`${mode}-log`),
      // The batches sent before `load`, or null once it ran.
      waiting: [],
      prompt: null,
      frame: null,
    };
    consoles.set(mode, con);
    reset(con, 0, 0);
  }

  /** Empties `con` for `generation`, its first line at `first`, and shows the empty prompt. */
  // `lines`: the lines kept, drawn or not, as the core sent them; `first` is the absolute index of
  // `lines[0]`; `pending`: how many of the newest `lines` wait for the next frame.
  function reset(con, generation, first) {
    if (con.frame !== null) cancelFrame(con.frame);
    Object.assign(con, { generation, first, lines: [], pending: 0, frame: null, errors: 0 });
    if (!con.el) return;
    con.prompt = doc.createElement("div");
    con.prompt.className = "console-empty-prompt";
    con.prompt.textContent = EMPTY_PROMPT;
    con.el.replaceChildren(con.prompt);
  }

  /** Draws the pending lines of `con`, following the scroll when it was at the bottom. */
  function flush(con) {
    con.frame = null;
    const el = con.el;
    if (!el || con.pending === 0) return;
    con.prompt?.remove();
    con.prompt = null;
    const frag = doc.createDocumentFragment();
    for (const line of con.lines.slice(-con.pending)) {
      const div = doc.createElement("div");
      div.className = `log-line ${lineClass(line.kind)}`;
      // Paths display Finder-style; Copy uses the raw text.
      div.textContent = line.display ?? line.text;
      frag.appendChild(div);
    }
    con.pending = 0;
    const shouldFollow = el.scrollHeight - el.scrollTop - el.clientHeight < 32;
    el.appendChild(frag);
    if (shouldFollow) el.scrollTop = el.scrollHeight;
  }

  function draw(con, batch) {
    if (batch.generation < con.generation) return;
    const newer = batch.generation > con.generation;
    if (newer) reset(con, batch.generation, batch.first);
    const end = con.first + con.lines.length;
    // A gap means lines were never sent here; index on from this batch.
    if (batch.first > end) con.first = batch.first - con.lines.length;
    // Lines this console already holds are not drawn again.
    const fresh = batch.lines.slice(Math.max(0, end - batch.first));
    con.lines.push(...fresh);
    con.pending += fresh.length;
    // The core keeps nothing below `keep_from`: drop those lines, drawn ones first.
    const drop = Math.min(con.lines.length, Math.max(0, batch.keep_from - con.first));
    if (drop > 0) {
      const drawn = con.lines.length - con.pending;
      const undrawn = Math.max(0, drop - drawn);
      for (let i = Math.min(drop, drawn); i > 0; i--) con.el?.firstElementChild?.remove();
      con.pending -= undrawn;
      con.lines.splice(0, drop);
      con.first += drop;
    }
    // A batch that adds nothing (one `load` replays under the view) keeps the newer count.
    if (newer || fresh.length > 0) con.errors = batch.errors;
    if (con.pending > 0 && con.frame === null) con.frame = requestFrame(() => flush(con));
  }

  return {
    apply(batch) {
      const con = consoles.get(batch?.mode);
      if (!con) return;
      if (con.waiting) con.waiting.push(batch);
      else draw(con, batch);
      onErrors();
    },
    /** Draws `view` (`get_log`, or null when it failed), then the batches that waited for it. */
    load(mode, view) {
      const con = consoles.get(mode);
      if (!con?.waiting) return;
      const waiting = con.waiting;
      con.waiting = null;
      if (view) draw(con, { ...view, keep_from: view.first });
      for (const batch of waiting) draw(con, batch);
      onErrors();
    },
    text: mode => (consoles.get(mode)?.lines ?? []).map(line => line.text).join("\n"),
    errors: mode => consoles.get(mode)?.errors ?? 0,
  };
}
