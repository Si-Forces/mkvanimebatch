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
