(function () {
  const ua = (navigator.userAgent || "").toLowerCase();
  const platform = (navigator.platform || "").toLowerCase();
  const root = document.documentElement;
  const match = [
    ["win", "is-windows"],
    ["mac", "is-mac"],
    ["linux", "is-linux"],
  ].find(([token]) => ua.includes(token) || platform.includes(token));

  if (match) {
    root.classList.add(match[1]);
  }
})();
