// Maps engine log lines to console CSS classes. Cosmetic only: no behaviour keys off these classes.

export function stripLogPrefix(text) {
  return text.replace(/^(\[ERR\]\s*)?\d{4}-\d{2}-\d{2}[ T][\d:]+[.,]\d+\s+\[\w+\]\s+/, "$1");
}

// Ordered rules: first match wins. `includes` tests substrings of the
// lowercased stripped line; `startsWith` tests its prefix.
const RULES = [
  { class: "line-err", includes: ["crashed", "[-]", "[err]", "error", "failed"] },
  { class: "line-skip", includes: ["skipped", "skip", "already exists", "already complete"] },
  {
    class: "line-ok",
    includes: ["[+]", "success", "finished", "completed", "remuxed:", "extracted:", "remuxed cleanly", "extracted cleanly"],
  },
  { class: "line-warn", includes: ["warn"] },
  { class: "line-info", includes: ["directory:", "dry run"], startsWith: ["processing", "extracting"] },
  { class: "line-done", includes: ["persistent inspection cache", "cache cleared"] },
];

export function classifyLine(text) {
  const t = stripLogPrefix(text).toLowerCase();
  for (const rule of RULES) {
    if (rule.includes && rule.includes.some((needle) => t.includes(needle))) return rule.class;
    if (rule.startsWith && rule.startsWith.some((needle) => t.startsWith(needle))) return rule.class;
  }
  return "";
}
