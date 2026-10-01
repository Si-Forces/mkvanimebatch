// Applies data-theme before the stylesheet paints. Mirrors initTheme() in app.js (same key and
// OS-default rule). Must stay an external classic script: the CSP is `script-src 'self'`.
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
