// Sole resolver of the initial theme, applied before the stylesheet paints. app.js does NOT
// re-derive this: it reads the `dataset.theme` / `dataset.themeExplicit` published below, so
// this file is the only place that reads `localStorage` for the theme. app.js reads
// `matchMedia` only to follow live OS changes, or if this script never ran.
// Must stay an external classic script: the CSP is `script-src 'self'`.
(function () {
  const stored = (() => {
    try {
      return localStorage.getItem("mkv-theme");
    } catch {
      return null;
    }
  })();
  const osDark = window.matchMedia?.("(prefers-color-scheme: dark)")?.matches;
  const isExplicit = stored === "light" || stored === "dark";
  document.documentElement.dataset.theme =
    isExplicit ? stored : osDark === false ? "light" : "dark";
  document.documentElement.dataset.themeExplicit = isExplicit ? "true" : "false";
})();
