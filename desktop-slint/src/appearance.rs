//! The appearance choices (P6): the theme picked with the toggle or Ctrl+T (theme.js), or in the
//! title bar's Appearance menu, whose System follows the system's scheme live again; and the in-app
//! reduced effects that stand in for the webview's `prefers-reduced-transparency` and
//! `prefers-reduced-motion` (Q105). All three are saved together in
//! `<config dir>/com.mkvanimebatch.desktop/appearance` (Q96), as `key=value` lines. Unset effects
//! are saved as `system` and seeded once at startup from the OS preference (never written as a
//! choice). The webview's `mkv-theme` localStorage key is not migrated.

use crate::renderer::Renderer;
use mkv_anime_batch_slint_ui::{App, Theme};
use slint::language::ColorScheme;
use slint::winit_030::winit::window::Theme as WindowTheme;
use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;
use std::cell::Cell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The saved choices, beside the renderer choice (`renderer.rs`).
const FILE: &str = "appearance";

/// OS-reported reduced-effects preferences (read synchronously at startup on Windows; reduced
/// motion arrives asynchronously from the portal on Linux), never written to disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OsEffects {
    pub(crate) reduce_transparency: bool,
    pub(crate) reduce_motion: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Appearance {
    /// The theme the user picked: dark or light; `None` follows the system.
    pub(crate) dark: Option<bool>,
    /// Tri-state for both effects: `Some` is a saved choice; `None` (`system`) falls back to the
    /// OS preference, else off.
    pub(crate) reduce_transparency: Option<bool>,
    pub(crate) reduce_motion: Option<bool>,
}

impl Appearance {
    /// Reads `key=value` lines; `system`, a missing key, an unknown key or an unreadable value
    /// stays unset (`None`).
    fn parse(text: &str) -> Self {
        let mut appearance = Self::default();
        for (key, value) in text
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.trim(), value.trim()))
        {
            let flag = match value {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
            match key {
                "theme" => {
                    appearance.dark = match value {
                        "dark" => Some(true),
                        "light" => Some(false),
                        _ => None,
                    }
                }
                "reduce-transparency" => appearance.reduce_transparency = flag,
                "reduce-motion" => appearance.reduce_motion = flag,
                _ => {}
            }
        }
        appearance
    }

    fn effect_text(choice: Option<bool>) -> &'static str {
        match choice {
            Some(true) => "true",
            Some(false) => "false",
            None => "system",
        }
    }

    fn text(&self) -> String {
        let theme = match self.dark {
            Some(true) => "dark",
            Some(false) => "light",
            None => "system",
        };
        format!(
            "theme={theme}\nreduce-transparency={}\nreduce-motion={}\n",
            Self::effect_text(self.reduce_transparency),
            Self::effect_text(self.reduce_motion),
        )
    }

    /// The choices saved in `dir`; none (all unset) when the file is missing or unreadable.
    pub(crate) fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(FILE))
            .map_or_else(|_| Self::default(), |t| Self::parse(&t))
    }

    fn save(&self, dir: &Path) -> io::Result<()> {
        crate::renderer::save_file(dir, FILE, &self.text())
    }

    fn effective_reduce_transparency(&self, os: OsEffects) -> bool {
        self.reduce_transparency.unwrap_or(os.reduce_transparency)
    }

    fn effective_reduce_motion(&self, os: OsEffects) -> bool {
        self.reduce_motion.unwrap_or(os.reduce_motion)
    }

    /// `Theme.glass`: whether the panes draw the real backdrop blur, on the GPU renderer unless
    /// transparency is reduced, and only while [`REAL_BLUR`] is on. Otherwise they draw the faked
    /// glass (ADR-0017 decision 3), solid under Reduce transparency through the theme's tokens.
    pub(crate) fn glass(&self, renderer: Renderer, os: OsEffects) -> bool {
        REAL_BLUR && self.can_blur(renderer, os)
    }

    /// Whether the real blur could draw: on the GPU renderer, with full transparency.
    fn can_blur(&self, renderer: Renderer, os: OsEffects) -> bool {
        renderer == Renderer::OpenGl && !self.effective_reduce_transparency(os)
    }
}

/// The real backdrop blur, off: the maintainer chose the webview's look on 2026-10-27, the faked
/// glass whose rows show through sharp, where a σ30 blur smears them into one colour; it also
/// missed ADR-0017 decision 8's 1% low on the HD 4000 (#142). P9's gate decides whether it comes
/// back; the bench draws it with `MKV_BENCH_BLUR=1`.
const REAL_BLUR: bool = false;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct State {
    saved: Appearance,
    os: OsEffects,
    /// An OS reduced-motion reply that came while the Appearance menu was open, held until it
    /// closes.
    pending_motion: Option<bool>,
}

/// Applies an asynchronous OS reduced-motion seed to `Theme` only while the user has not saved a
/// choice. While the Appearance menu is open it is held, touching neither `state.os` nor
/// `Theme`, and applied as the menu closes: a checkbox never flips under the pointer (a release
/// would toggle the seeded value and save the opposite of the click), and `state.os` keeps
/// matching `Theme`, so a click on the other checkbox cannot save a stale value.
pub(crate) fn seed_motion(app: &App, state: &Cell<State>, reduce_motion: bool) {
    let mut next = state.get();
    if app.get_appearance_open() {
        next.pending_motion = Some(reduce_motion);
        state.set(next);
        return;
    }
    next.os.reduce_motion = reduce_motion;
    state.set(next);
    if next.saved.reduce_motion.is_none() {
        app.global::<Theme>().set_reduce_motion(reduce_motion);
    }
}

/// Applies `appearance` (falling back to `os` for unset effects) to the window's theme. Call it
/// before the first frame: a picked theme then pins `StylePalette.color-scheme` from the start,
/// so the system's never shows first.
fn apply(app: &App, appearance: &Appearance, os: OsEffects, renderer: Renderer) {
    set_theme(app, appearance.dark);
    let theme = app.global::<Theme>();
    theme.set_reduce_transparency(appearance.effective_reduce_transparency(os));
    theme.set_reduce_motion(appearance.effective_reduce_motion(os));
    theme.set_glass(appearance.glass(renderer, os));
}

/// Pins the palette's scheme to the picked theme, or (`None`) lets it follow the system's live:
/// Slint's `unknown` scheme.
fn set_theme(app: &App, dark: Option<bool>) {
    let theme = app.global::<Theme>();
    match dark {
        Some(dark) => theme.invoke_set_dark(dark),
        None => theme.invoke_follow_system(),
    }
}

/// Pins the native window's theme to the picked one (window.rs `set_window_theme`), or (`None`)
/// unpins it, so winit follows the system's again: the window is frameless, so it only matters to
/// the compositor and native dialogs. Nothing before the window exists.
fn pin_window_theme(app: &App, dark: Option<bool>) {
    let theme = dark.map(|dark| {
        if dark {
            WindowTheme::Dark
        } else {
            WindowTheme::Light
        }
    });
    app.window()
        .with_winit_window(|window| window.set_theme(theme));
}

/// The theme `dark` picked (the toggle, Ctrl+T, the menu): applied to the palette and the window,
/// and saved with the saved effects (leaving unset effects as `system`).
fn choose(app: &App, state: &Cell<State>, dir: Option<&Path>, dark: Option<bool>) {
    set_theme(app, dark);
    pin_window_theme(app, dark);
    let mut next = state.get();
    next.saved.dark = dark;
    state.set(next);
    save(dir, &next.saved);
}

/// Saves `appearance` in `dir`, if there is one; a failure is logged, and the choice still holds
/// until the app closes.
fn save(dir: Option<&Path>, appearance: &Appearance) {
    match dir.map(|dir| (dir, appearance.save(dir))) {
        Some((_, Ok(()))) => {}
        Some((dir, Err(error))) => tracing::warn!(
            "Cannot save the appearance choices in {}: {error}",
            dir.display()
        ),
        None => tracing::warn!("No config directory: the appearance choices are not saved"),
    }
}

/// Applies the choices saved in `config` (seeded with the platform's OS preferences for unset
/// effects), before the first frame, then wires the toggle (and Ctrl+T), the menu's theme choice
/// and its reduced-effects checkboxes. On Linux, the XDG Settings portal read runs as an async
/// task on the event loop so it never delays the first frame.
pub(crate) fn wire(app: &App, config: Option<PathBuf>, renderer: Renderer) {
    let state = wire_with_os(app, config, renderer, os_effects_at_start());
    #[cfg(target_os = "linux")]
    spawn_linux_motion_seed(app, state);
    #[cfg(not(target_os = "linux"))]
    let _ = state;
}

/// Wires appearance with an explicit startup `os` seed, returning the shared state for any
/// asynchronous reduced-motion update.
pub(crate) fn wire_with_os(
    app: &App,
    config: Option<PathBuf>,
    renderer: Renderer,
    os: OsEffects,
) -> Rc<Cell<State>> {
    let saved = config.as_deref().map(Appearance::load).unwrap_or_default();
    apply(app, &saved, os, renderer);
    let state = Rc::new(Cell::new(State {
        saved,
        os,
        pending_motion: None,
    }));
    if let Some(dark) = saved.dark {
        // Once winit made the window (as main's `centre` waits for it).
        let window = app.as_weak();
        let pinned = slint::spawn_local(async move {
            let Some(app) = window.upgrade() else {
                return;
            };
            if app.window().winit_window().await.is_ok() {
                pin_window_theme(&app, Some(dark));
            }
        });
        if let Err(error) = pinned {
            tracing::warn!("Cannot pin the window theme: {error}");
        }
    }

    let theme = app.global::<Theme>();
    theme.on_toggle({
        let (window, state, dir) = (app.as_weak(), Rc::clone(&state), config.clone());
        move || {
            if let Some(app) = window.upgrade() {
                let dark = !app.global::<Theme>().get_dark();
                choose(&app, &state, dir.as_deref(), Some(dark));
            }
        }
    });
    theme.on_pick({
        let (window, state, dir) = (app.as_weak(), Rc::clone(&state), config.clone());
        move |scheme| {
            if let Some(app) = window.upgrade() {
                let dark = match scheme {
                    ColorScheme::Dark => Some(true),
                    ColorScheme::Light => Some(false),
                    _ => None,
                };
                choose(&app, &state, dir.as_deref(), dark);
            }
        }
    });
    app.on_appearance_closed({
        let (window, state) = (app.as_weak(), Rc::clone(&state));
        move || {
            let mut next = state.get();
            let Some(reduce_motion) = next.pending_motion.take() else {
                return;
            };
            state.set(next);
            if let Some(app) = window.upgrade() {
                seed_motion(&app, &state, reduce_motion);
            }
        }
    });
    let window = app.as_weak();
    theme.on_effects_changed({
        let state = Rc::clone(&state);
        move || {
            let Some(app) = window.upgrade() else {
                return;
            };
            let theme = app.global::<Theme>();
            let (new_transparency, new_motion) =
                (theme.get_reduce_transparency(), theme.get_reduce_motion());
            let mut next = state.get();
            let mut changed = false;
            // Only promote the effect whose checkbox actually changed into a saved choice,
            // so clicking one checkbox keeps the other unset (`system`) if it was unset.
            if new_transparency != next.saved.effective_reduce_transparency(next.os) {
                next.saved.reduce_transparency = Some(new_transparency);
                changed = true;
            }
            if new_motion != next.saved.effective_reduce_motion(next.os) {
                next.saved.reduce_motion = Some(new_motion);
                changed = true;
            }
            if changed {
                state.set(next);
                theme.set_glass(next.saved.glass(renderer, next.os));
                save(config.as_deref(), &next.saved);
            }
        }
    });
    state
}

#[cfg(any(target_os = "linux", test))]
fn portal_reduces_motion(reduced_motion: Option<u32>, enable_animations: Option<bool>) -> bool {
    reduced_motion == Some(1) || enable_animations == Some(false)
}

#[cfg(any(target_os = "windows", test))]
fn registry_reduces_transparency(enable_transparency: Option<u32>) -> bool {
    enable_transparency == Some(0)
}

/// Reads one portal key with `ReadOne`, falling back to the older `Read` (whose extra variant
/// wrapping `downcast_ref` unwraps).
#[cfg(target_os = "linux")]
async fn read_portal_setting(
    proxy: &zbus::Proxy<'_>,
    namespace: &str,
    key: &str,
) -> Option<zbus::zvariant::OwnedValue> {
    let args = (namespace, key);
    let result = match proxy.call("ReadOne", &args).await {
        Err(zbus::Error::MethodError(name, ..))
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
        {
            proxy.call("Read", &args).await
        }
        result => result,
    };
    result.ok()
}

/// Queries the XDG Settings portal for reduced motion: either the standard `reduced-motion == 1`
/// (portal 1.21+: Plasma 6.6+, GNOME 50+) or GNOME/GTK's `enable-animations == false` reduces it.
#[cfg(target_os = "linux")]
async fn query_linux_motion() -> zbus::Result<bool> {
    let connection = zbus::Connection::session().await?;
    let proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(&connection)
        .interface("org.freedesktop.portal.Settings")?
        .path("/org/freedesktop/portal/desktop")?
        .destination("org.freedesktop.portal.Desktop")?
        .build()
        .await?;
    let reduced_motion =
        read_portal_setting(&proxy, "org.freedesktop.appearance", "reduced-motion")
            .await
            .and_then(|value| value.downcast_ref::<u32>().ok());
    let enable_animations =
        read_portal_setting(&proxy, "org.gnome.desktop.interface", "enable-animations")
            .await
            .and_then(|value| value.downcast_ref::<bool>().ok());
    Ok(portal_reduces_motion(reduced_motion, enable_animations))
}

/// Spawns the Linux portal query on Slint's event loop (`zbus` drives its `async-io` socket off
/// the UI thread, like Slint's own portal watcher), skipping it when a choice is already saved.
#[cfg(target_os = "linux")]
fn spawn_linux_motion_seed(app: &App, state: Rc<Cell<State>>) {
    if state.get().saved.reduce_motion.is_some() {
        return;
    }
    let window = app.as_weak();
    let spawned = slint::spawn_local(async move {
        match query_linux_motion().await {
            Ok(true) => {
                if let Some(app) = window.upgrade() {
                    seed_motion(&app, &state, true);
                }
            }
            Ok(false) => {}
            Err(error) => {
                tracing::debug!(
                    "Cannot read the reduced-motion preference from the XDG Settings portal: {error}"
                );
            }
        }
    });
    if let Err(error) = spawned {
        tracing::warn!("Cannot query OS reduced-motion preference: {error}");
    }
}

/// Reads HKCU `EnableTransparency` (DWORD == 0 reduces transparency, as Chromium/WebView2 read it).
#[cfg(target_os = "windows")]
fn read_windows_transparency() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let value: Option<u32> = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_value("EnableTransparency"))
        .ok();
    registry_reduces_transparency(value)
}

/// Reads Slint's motion preference, populated synchronously from `SPI_GETCLIENTAREAANIMATION` in
/// winit's `Backend::bind_context` when `BackendSelector::select` (or `App::new`) sets the platform.
#[cfg(target_os = "windows")]
fn read_windows_motion() -> bool {
    use i_slint_core::context::{with_existing_context, MotionPreference};
    with_existing_context(|ctx| ctx.motion_preference() == MotionPreference::Reduced)
        .unwrap_or(false)
}

/// OS reduced-effects preferences available synchronously before the first frame.
#[cfg(target_os = "windows")]
fn os_effects_at_start() -> OsEffects {
    OsEffects {
        reduce_transparency: read_windows_transparency(),
        reduce_motion: read_windows_motion(),
    }
}

/// OS reduced-effects preferences available synchronously before the first frame (none here; Linux
/// seeds reduced motion asynchronously from the portal).
#[cfg(not(target_os = "windows"))]
fn os_effects_at_start() -> OsEffects {
    OsEffects::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choices_survive_a_save_and_a_load() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("com.mkvanimebatch.desktop");
        for appearance in [
            Appearance {
                dark: Some(true),
                reduce_transparency: Some(true),
                reduce_motion: Some(false),
            },
            Appearance {
                dark: Some(false),
                reduce_transparency: Some(false),
                reduce_motion: Some(true),
            },
            Appearance {
                dark: Some(true),
                reduce_transparency: None,
                reduce_motion: None,
            },
            Appearance::default(),
        ] {
            appearance.save(&config).unwrap();
            assert_eq!(Appearance::load(&config), appearance);
        }
        // Unset effects write `system`, never a concrete boolean.
        let text = std::fs::read_to_string(config.join(FILE)).unwrap();
        assert_eq!(
            text,
            "theme=system\nreduce-transparency=system\nreduce-motion=system\n"
        );
    }

    #[test]
    fn a_missing_or_corrupt_file_follows_the_system_with_every_effect() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Appearance::load(dir.path()), Appearance::default());
        // Not UTF-8: unreadable as text.
        std::fs::write(dir.path().join(FILE), [0xff, 0xfe, 0x00, 0x9f]).unwrap();
        assert_eq!(Appearance::load(dir.path()), Appearance::default());
        // `system`, missing keys, garbage and unreadable values stay unset; `true`/`false` stay choices.
        std::fs::write(
            dir.path().join(FILE),
            "garbage\ntheme=purple\nreduce-transparency=yes\nreduce-motion=true\n",
        )
        .unwrap();
        assert_eq!(
            Appearance::load(dir.path()),
            Appearance {
                reduce_motion: Some(true),
                ..Appearance::default()
            }
        );
        std::fs::write(
            dir.path().join(FILE),
            "theme=dark\nreduce-transparency=system\nreduce-motion=false\n",
        )
        .unwrap();
        assert_eq!(
            Appearance::load(dir.path()),
            Appearance {
                dark: Some(true),
                reduce_transparency: None,
                reduce_motion: Some(false),
            }
        );
        // Omitted effect keys stay unset (`None`), never `Some(false)`.
        std::fs::write(dir.path().join(FILE), "theme=dark\n").unwrap();
        assert_eq!(
            Appearance::load(dir.path()),
            Appearance {
                dark: Some(true),
                reduce_transparency: None,
                reduce_motion: None,
            }
        );
    }

    #[test]
    fn saved_choice_beats_os_preference_which_beats_off() {
        let os_off = OsEffects::default();
        let os_on = OsEffects {
            reduce_transparency: true,
            reduce_motion: true,
        };
        for (saved, os, expected) in [
            (None, false, false),
            (None, true, true),
            (Some(false), false, false),
            (Some(false), true, false),
            (Some(true), false, true),
            (Some(true), true, true),
        ] {
            let appearance = Appearance {
                dark: None,
                reduce_transparency: saved,
                reduce_motion: saved,
            };
            let effects = if os { os_on } else { os_off };
            assert_eq!(
                appearance.effective_reduce_transparency(effects),
                expected,
                "transparency saved={saved:?} os={os}"
            );
            assert_eq!(
                appearance.effective_reduce_motion(effects),
                expected,
                "motion saved={saved:?} os={os}"
            );
        }
    }

    #[test]
    fn the_glass_needs_the_real_blur_the_gpu_and_full_transparency() {
        let os_off = OsEffects::default();
        let os_solid = OsEffects {
            reduce_transparency: true,
            reduce_motion: false,
        };
        let saved_on = Appearance {
            reduce_transparency: Some(true),
            ..Appearance::default()
        };
        let saved_off = Appearance {
            reduce_transparency: Some(false),
            ..Appearance::default()
        };
        assert_eq!(
            Appearance::default().glass(Renderer::OpenGl, os_off),
            REAL_BLUR
        );
        assert!(Appearance::default().can_blur(Renderer::OpenGl, os_off));
        assert!(!Appearance::default().can_blur(Renderer::Software, os_off));
        assert!(!saved_on.can_blur(Renderer::OpenGl, os_off));
        assert!(!Appearance::default().can_blur(Renderer::OpenGl, os_solid));
        assert!(saved_off.can_blur(Renderer::OpenGl, os_solid));
        assert!(!Appearance::default().glass(Renderer::Software, os_off));
        assert!(!saved_on.glass(Renderer::OpenGl, os_off));
        assert!(!saved_on.glass(Renderer::Software, os_off));
        assert!(!Appearance::default().glass(Renderer::OpenGl, os_solid));
    }

    #[test]
    fn portal_mapping_reduces_motion_on_either_key() {
        for (reduced_motion, enable_animations, expected) in [
            (Some(1), None, true),
            (Some(1), Some(true), true),
            (Some(0), Some(false), true),
            (None, Some(false), true),
            (Some(0), Some(true), false),
            (Some(0), None, false),
            (None, Some(true), false),
            (None, None, false),
            (Some(2), Some(true), false),
        ] {
            assert_eq!(
                portal_reduces_motion(reduced_motion, enable_animations),
                expected,
                "reduced_motion={reduced_motion:?} enable_animations={enable_animations:?}"
            );
        }
    }

    #[test]
    fn registry_mapping_reduces_transparency_only_on_zero() {
        assert!(registry_reduces_transparency(Some(0)));
        assert!(!registry_reduces_transparency(Some(1)));
        assert!(!registry_reduces_transparency(Some(2)));
        assert!(!registry_reduces_transparency(None));
    }
}
