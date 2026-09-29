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
  document.documentElement.dataset.theme =
    stored === "light" || stored === "dark" ? stored : osDark === false ? "light" : "dark";
})();
