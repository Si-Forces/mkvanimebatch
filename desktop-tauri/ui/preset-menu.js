/**
 * The titlebar's Preset Profile menu: applies a profile's name, bitrate and complexity to the
 * form, or marks the form as hand-edited ("custom"). The engine owns the profile catalog and
 * `load()` fetches it; the page keeps no copy to fall back on.
 */
export function createPresetMenu({ invoke, doc }) {
  // Keyed by profile id; empty until `load()` has an answer.
  const catalog = {};

  async function load() {
    try {
      const profiles = await invoke("get_preset_profiles");
      for (const p of profiles ?? []) {
        catalog[p.id] = {
          icon: p.icon,
          name: p.name,
          bitrate: p.bitrate,
          complexity: String(p.complexity),
        };
      }
    } catch (err) {
      console.error("[get_preset_profiles] failed:", err);
    }
  }

  function setActiveProfileOption(presetKey) {
    doc.querySelectorAll(".profile-option-item").forEach(item => {
      const active = item.dataset.preset === presetKey;
      item.classList.toggle("active", active);
      item.setAttribute("aria-pressed", active ? "true" : "false");
    });
  }

  function apply(presetKey) {
    const p = catalog[presetKey];
    const nameEl = doc.getElementById("active-profile-name");
    const profileBtn = doc.getElementById("active-profile-btn");

    if (presetKey === "custom") {
      if (nameEl) nameEl.textContent = "🛠️ Custom Configuration";
      if (profileBtn) profileBtn.dataset.preset = "custom";
      setActiveProfileOption("custom");
      return;
    }
    // Without its catalog entry there is nothing to apply.
    if (!p) return;

    if (nameEl) nameEl.textContent = `${p.icon} ${p.name}`;
    if (profileBtn) profileBtn.dataset.preset = presetKey;
    setActiveProfileOption(presetKey);

    const setVal = (id, val) => {
      const el = doc.getElementById(id);
      if (el) el.value = val;
    };

    if (p.bitrate !== undefined) setVal("remux-bitrate", p.bitrate);
    if (p.complexity !== undefined) {
      setVal("remux-complexity", p.complexity);
      setVal("remux-cplx-val", p.complexity);
    }
  }

  return { load, apply };
}
