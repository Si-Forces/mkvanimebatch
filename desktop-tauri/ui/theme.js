const THEME_KEY = "mkv-theme";

/**
 * The light/dark theme after startup: `init()` applies the choice `theme-init.js` published, or
 * follows the OS color scheme live until the user picks one; `toggle()` flips it. `win` gives
 * `matchMedia`, and `storage.setItem` saves a choice (it may throw). `invoke` pins the native
 * window theme.
 */
export function createTheme({ doc, win, storage, invoke }) {
  // The last explicitly chosen theme; a blocked or full storage must not lose the choice.
  let chosenTheme = null;

  function setDocumentTheme(theme) {
    doc.documentElement.setAttribute("data-theme", theme);
  }

  /**
   * An explicit choice also pins the native window theme; following the OS leaves it unpinned.
   * It sets `dataset.themeExplicit` too, so the DOM stays the single source of truth after a
   * toggle and agrees with storage.
   */
  function applyChosenTheme(theme) {
    setDocumentTheme(theme);
    doc.documentElement.dataset.themeExplicit = "true";
    invoke("set_window_theme", { theme });
  }

  // The explicit choice `theme-init.js` published, or null to follow the OS. Never re-read
  // localStorage here: `theme-init.js` is its sole reader, so the two copies cannot disagree.
  function storedTheme() {
    const dataset = doc.documentElement?.dataset;
    if (dataset?.themeExplicit !== "true") return null;
    return dataset.theme === "light" || dataset.theme === "dark" ? dataset.theme : null;
  }

  /** Applies the user's stored theme, or follows the OS color scheme live until they pick one. */
  function init() {
    const saved = storedTheme();
    if (saved) {
      applyChosenTheme(saved);
      return;
    }
    const darkScheme = win.matchMedia?.("(prefers-color-scheme: dark)");
    const follow = dark => {
      if (!chosenTheme && !storedTheme()) setDocumentTheme(dark ? "dark" : "light");
    };
    if (!doc.documentElement?.dataset?.theme) {
      follow(darkScheme?.matches ?? true);
    }
    darkScheme?.addEventListener("change", e => follow(e.matches));
  }

  function toggle() {
    const theme = doc.documentElement.getAttribute("data-theme") === "dark" ? "light" : "dark";
    chosenTheme = theme;
    try {
      storage.setItem(THEME_KEY, theme);
    } catch {}
    applyChosenTheme(theme);
  }

  return { init, toggle };
}
