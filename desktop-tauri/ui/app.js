/* ─── MKV Anime Batch — Frontend App ──────────────────────────────────────
   Uses window.__TAURI__ (withGlobalTauri: true in tauri.conf.json)
──────────────────────────────────────────────────────────────────────────── */

"use strict";

// ─── Tauri API helpers ──────────────────────────────────────────────────────

async function invoke(cmd, args = {}) {
  try {
    const fn = window.__TAURI__?.core?.invoke
      || window.__TAURI__?.tauri?.invoke
      || window.__TAURI__?.invoke
      || window.__TAURI_INTERNALS__?.invoke;

    if (fn) {
      return await fn(cmd, args);
    }
    console.log("[mock invoke]", cmd, args);
    return null;
  } catch (err) {
    console.error(`[IPC Error] ${cmd}:`, err);
    return null;
  }
}

async function listen(event, handler) {
  try {
    const fn = window.__TAURI__?.event?.listen || window.__TAURI__?.listen;
    if (fn) {
      return await fn(event, handler);
    }
    return () => {};
  } catch (err) {
    console.error(`[Listen Error] ${event}:`, err);
    return () => {};
  }
}

function safeAddListener(idOrEl, event, handler) {
  const el = typeof idOrEl === "string" ? document.getElementById(idOrEl) : idOrEl;
  if (el) {
    el.addEventListener(event, handler);
  }
}

// ─── Number Stepper Helper ──────────────────────────────────────────────────
window.stepNumber = function(btn, dir) {
  const input = btn.parentElement.querySelector('input[type="number"]');
  if (!input) return;
  
  const step = parseFloat(input.getAttribute('step') || '1');
  const min = parseFloat(input.getAttribute('min'));
  const max = parseFloat(input.getAttribute('max'));
  let val = parseFloat(input.value) || 0;
  
  val += dir * step;
  if (!isNaN(min) && val < min) val = min;
  if (!isNaN(max) && val > max) val = max;
  
  input.value = parseFloat(val.toPrecision(10));
  input.dispatchEvent(new Event('input'));
};

document.addEventListener('DOMContentLoaded', () => {
  let stepInterval = null;
  let stepTimeout = null;

  document.querySelectorAll('.stepper-btn').forEach(btn => {
    const dir = btn.classList.contains('minus') ? -1 : 1;
    
    const startStepping = () => {
      window.stepNumber(btn, dir);
      stepTimeout = setTimeout(() => {
        stepInterval = setInterval(() => window.stepNumber(btn, dir), 80);
      }, 400);
    };
    
    const stopStepping = () => {
      clearTimeout(stepTimeout);
      clearInterval(stepInterval);
    };
    
    btn.addEventListener('mousedown', startStepping);
    btn.addEventListener('mouseup', stopStepping);
    btn.addEventListener('mouseleave', stopStepping);
  });
});

// ─── Theme ──────────────────────────────────────────────────────────────────

const THEME_KEY = "mkv-theme";

function applyTheme(theme) {
  document.documentElement.setAttribute("data-theme", theme);
  const btn = document.getElementById("btn-theme");
  if (btn) btn.textContent = theme === "dark" ? "🌙" : "☀️";
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch (e) {}
}

function initTheme() {
  let saved = null;
  try {
    saved = localStorage.getItem(THEME_KEY);
  } catch (e) {}

  if (saved) {
    applyTheme(saved);
    return;
  }
  const prefersDark = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
  applyTheme(prefersDark ? "dark" : "light");
}

// ─── Navigation ──────────────────────────────────────────────────────────────

function switchView(viewId) {
  document.querySelectorAll(".view").forEach(v => v.classList.remove("active"));
  document.querySelectorAll(".nav-item").forEach(n => n.classList.remove("active"));
  const view = document.getElementById(`view-${viewId}`);
  const nav  = document.getElementById(`nav-${viewId}`);
  if (view) {
    view.classList.add("active");
  }
  if (nav)  nav.classList.add("active");
}

// ─── Log Console helpers ──────────────────────────────────────────────────────

function classifyLine(text) {
  const t = text.toLowerCase();
  if (t.includes("[+]") || t.includes("success") || t.includes("finished") || t.includes("completed")) return "line-ok";
  if (t.includes("[-]") || t.includes("[err]") || t.includes("error") || t.includes("failed") || t.includes("[error]")) return "line-err";
  if (t.includes("warn") || t.includes("[warn]")) return "line-warn";
  if (t.includes("skip") || t.includes("already complete")) return "line-skip";
  if (t.includes("folder:") || t.includes("dry run") || t.startsWith("extracting") || t.startsWith("processing")) return "line-info";
  if (t.includes("persistent inspection cache") || t.includes("cache cleared")) return "line-done";
  return "";
}

function appendLog(logEl, text) {
  if (!logEl) return;
  const placeholder = logEl.querySelector(".empty-log");
  if (placeholder) placeholder.remove();

  const div = document.createElement("div");
  div.className = `log-line ${classifyLine(text)}`;
  div.textContent = text;
  logEl.appendChild(div);
  logEl.scrollTop = logEl.scrollHeight;
}

function clearLog(logEl) {
  if (!logEl) return;
  logEl.innerHTML = `
    <div class="empty-log">
      <span class="empty-icon">📋</span>
      <span>No output yet — run a job to see logs here.</span>
    </div>`;
}

// ─── Job Controller ───────────────────────────────────────────────────────────

// ─── Modal Dialog ────────────────────────────────────────────────────────────

function showConfirmModal(title, message) {
  return new Promise(resolve => {
    const modal = document.getElementById("confirm-modal");
    const titleEl = document.getElementById("modal-title");
    const descEl = document.getElementById("modal-desc");
    const cancelBtn = document.getElementById("modal-cancel");
    const confirmBtn = document.getElementById("modal-confirm");

    if (!modal || !cancelBtn || !confirmBtn) {
      resolve(confirm(message));
      return;
    }

    if (titleEl) titleEl.textContent = title;
    if (descEl) descEl.textContent = message;

    modal.classList.add("active");

    const cleanup = (result) => {
      modal.classList.remove("active");
      cancelBtn.removeEventListener("click", onCancel);
      confirmBtn.removeEventListener("click", onConfirm);
      resolve(result);
    };

    const onCancel = () => cleanup(false);
    const onConfirm = () => cleanup(true);

    cancelBtn.addEventListener("click", onCancel);
    confirmBtn.addEventListener("click", onConfirm);
  });
}

class JobController {
  constructor(mode) {
    this.mode     = mode;           // "remux" | "extract"
    this.running  = false;
    this.unlisten = null;           // cleanup fn from listen()

    this.logEl     = document.getElementById(`${mode}-log`);
    this.statusEl  = document.getElementById(`${mode}-status`);
    this.exitCodeEl= document.getElementById(`${mode}-exit-code`);
    this.runBtn    = document.getElementById(`${mode}-run`);
    this.stopBtn   = document.getElementById(`${mode}-stop`);

    if (this.runBtn)  safeAddListener(this.runBtn,  "click", () => this.start());
    if (this.stopBtn) safeAddListener(this.stopBtn, "click", () => this.stop());

    const clearLogBtn = document.getElementById(`${mode}-clear-log`);
    if (clearLogBtn) safeAddListener(clearLogBtn, "click", () => clearLog(this.logEl));

    const clearCacheBtn = document.getElementById(`${mode}-clear-cache`);
    if (clearCacheBtn) safeAddListener(clearCacheBtn, "click", () => this.clearCache());
  }

  buildArgs() {
    const val = id => document.getElementById(`${this.mode}-${id}`)?.value?.trim() ?? "";
    const chk = id => document.getElementById(`${this.mode}-${id}`)?.checked ?? false;
    const args = [];

    if (this.mode === "remux") {
      const inputDir = val("input-dir");
      if (!inputDir) { alert("Input directory is required."); return null; }
      args.push("--input-dir", inputDir);
      const outDir = val("out-dir");     if (outDir)    args.push("--out-dir",   outDir);
      const fontsDir = val("fonts-dir"); if (fontsDir)  args.push("--fonts-dir", fontsDir);
      const assetsDir = val("assets-dir"); if (assetsDir) args.push("--assets-dir", assetsDir);
      const bitrate = val("bitrate");    if (bitrate)   args.push("--bitrate",   bitrate);
      const offset  = val("sub-offset"); if (offset && offset !== "0") args.push("--sub-offset", offset);
      const workers = val("workers");    if (workers)   args.push("--workers",   workers);
      const ioJobs  = val("io-jobs");    if (ioJobs)    args.push("--io-jobs",   ioJobs);
      const cplx    = val("complexity"); if (cplx)      args.push("--complexity",cplx);
      if (chk("dry-run"))   args.push("--dry-run");
      if (chk("keep-fonts"))args.push("--keep-fonts");
      if (chk("no-cache"))  args.push("--no-cache");
    }

    if (this.mode === "extract") {
      const inputDir = val("input-dir");
      if (!inputDir) { alert("Input directory is required."); return null; }
      args.push("--input-dir", inputDir);
      const outDir = val("out-dir");     if (outDir)    args.push("--out-dir",   outDir);
      const fontsDir = val("fonts-dir"); if (fontsDir)  args.push("--fonts-dir", fontsDir);
      const offset  = val("sub-offset"); if (offset && offset !== "0") args.push("--sub-offset", offset);
      const workers = val("workers");    if (workers) args.push("--workers", workers);
      const ioJobs  = val("io-jobs");    if (ioJobs)  args.push("--io-jobs",  ioJobs);
      if (chk("dry-run"))  args.push("--dry-run");
      if (chk("no-cache")) args.push("--no-cache");
    }

    return args;
  }

  async start() {
    if (this.running) return;
    const args = this.buildArgs();
    if (!args) return;

    this.setRunning(true);
    clearLog(this.logEl);
    if (this.exitCodeEl) this.exitCodeEl.textContent = "";

    if (this.unlisten) this.unlisten();
    this.unlisten = await listen("job-log", e => appendLog(this.logEl, e.payload));

    const unlistenDone = await listen("job-done", e => {
      const code = e.payload;
      this.onDone(code);
      unlistenDone();
    });

    try {
      await invoke("run_job", { mode: this.mode, args });
    } catch (err) {
      appendLog(this.logEl, `[ERROR] ${err}`);
      this.setRunning(false);
      unlistenDone();
    }
  }

  async stop() {
    try {
      await invoke("stop_job");
      appendLog(this.logEl, "[INFO] Job stopped by user.");
    } catch (err) {
      appendLog(this.logEl, `[ERROR] ${err}`);
    }
    this.setRunning(false);
  }

  async clearCache() {
    const confirmed = await showConfirmModal(
      "Clear Inspection Cache",
      `Are you sure you want to clear the persistent inspection cache for ${this.mode}?`
    );
    if (!confirmed) return;

    try {
      const msg = await invoke("clear_cache", { mode: this.mode });
      appendLog(this.logEl, `[CACHE] ${msg || "Cache cleared."}`);
    } catch (err) {
      appendLog(this.logEl, `[ERROR] ${err}`);
    }
  }

  onDone(code) {
    this.setRunning(false);
    if (this.exitCodeEl) this.exitCodeEl.textContent = `exit ${code}`;
  }

  setRunning(running) {
    this.running = running;
    if (this.runBtn)  this.runBtn.disabled  = running;
    if (this.stopBtn) this.stopBtn.disabled = !running;
    const el = this.statusEl;
    if (!el) return;
    el.className = "status-pill " + (running ? "status-running" : "status-idle");
    el.innerHTML = running
      ? `<span class="status-dot"></span> Running`
      : `<span class="status-dot"></span> Idle`;

    if (!running && this.exitCodeEl?.textContent.includes("exit 0")) {
      el.className = "status-pill status-success";
      el.innerHTML = `<span class="status-dot"></span> Done`;
    } else if (!running && this.exitCodeEl?.textContent && !this.exitCodeEl.textContent.includes("exit 0")) {
      el.className = "status-pill status-error";
      el.innerHTML = `<span class="status-dot"></span> Failed`;
    }
  }
}

// ─── Initialization ──────────────────────────────────────────────────────────

function initApp() {
  initTheme();

  // Detect OS for CSS tweaks
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("windows") || ua.includes("win32")) document.body.classList.add("is-windows");
  else if (ua.includes("mac")) document.body.classList.add("is-mac");
  else if (ua.includes("linux")) document.body.classList.add("is-linux");

  safeAddListener("btn-theme", "click", () => {
    const current = document.documentElement.getAttribute("data-theme");
    applyTheme(current === "dark" ? "light" : "dark");
  });

  safeAddListener("btn-close", "click", () => invoke("close_window"));
  safeAddListener("btn-min", "click", () => invoke("minimize_window"));
  safeAddListener("btn-max", "click", () => invoke("toggle_maximize"));

  document.querySelectorAll("[data-tauri-drag-region]").forEach(el => {
    safeAddListener(el, "mousedown", (e) => {
      // Ignore if clicking on interactive elements
      if (e.target.closest('button, .no-drag, input, a, select, textarea, .resize-handle')) {
        return;
      }
      if (e.buttons === 1) {
        invoke("start_drag");
      }
    });
  });

  document.querySelectorAll(".resize-handle").forEach(el => {
    safeAddListener(el, "mousedown", (e) => {
      if (e.buttons === 1 && el.dataset.direction) {
        invoke("start_resize", { direction: el.dataset.direction });
      }
    });
  });

  document.querySelectorAll(".nav-item[data-view]").forEach(btn => {
    safeAddListener(btn, "click", () => switchView(btn.dataset.view));
  });

  document.querySelectorAll(".browse-btn[data-target]").forEach(btn => {
    safeAddListener(btn, "click", async () => {
      const folder = await invoke("pick_folder");
      if (folder) {
        const input = document.getElementById(btn.dataset.target);
        if (input) input.value = folder;
      }
    });
  });

  new JobController("remux");
  new JobController("extract");

  if (window.matchMedia) {
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", e => {
      try {
        if (!localStorage.getItem(THEME_KEY)) applyTheme(e.matches ? "dark" : "light");
      } catch (err) {}
    });
  }

  document.addEventListener("keydown", e => {
    if ((e.ctrlKey || e.metaKey) && e.key === "t") {
      e.preventDefault();
      const cur = document.documentElement.getAttribute("data-theme");
      applyTheme(cur === "dark" ? "light" : "dark");
    }
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initApp);
} else {
  initApp();
}
