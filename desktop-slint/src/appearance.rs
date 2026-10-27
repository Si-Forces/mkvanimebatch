//! The appearance choices (P6): the theme picked with the toggle or Ctrl+T (theme.js), else the
//! system's, live; and the in-app reduced effects that stand in for the webview's
//! `prefers-reduced-transparency` and `prefers-reduced-motion` (Q105). All three are saved together
//! in `<config dir>/com.mkvanimebatch.desktop/appearance` (Q96), as `key=value` lines. The webview's
//! `mkv-theme` localStorage key is not migrated.

use crate::renderer::Renderer;
use mkv_anime_batch_slint_ui::{App, Theme};
use slint::winit_030::winit::window::Theme as WindowTheme;
use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;
use std::cell::Cell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The saved choices, beside the renderer choice (`renderer.rs`).
const FILE: &str = "appearance";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Appearance {
    /// The theme the user picked: dark or light; `None` follows the system.
    pub(crate) dark: Option<bool>,
    pub(crate) reduce_transparency: bool,
    pub(crate) reduce_motion: bool,
}

impl Appearance {
    /// Reads `key=value` lines; a missing key, an unknown one or a value it cannot read keeps the
    /// default (the system's theme, every effect on).
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
                "reduce-transparency" => appearance.reduce_transparency = flag.unwrap_or_default(),
                "reduce-motion" => appearance.reduce_motion = flag.unwrap_or_default(),
                _ => {}
            }
        }
        appearance
    }

    fn text(&self) -> String {
        let theme = match self.dark {
            Some(true) => "dark",
            Some(false) => "light",
            None => "system",
        };
        format!(
            "theme={theme}\nreduce-transparency={}\nreduce-motion={}\n",
            self.reduce_transparency, self.reduce_motion
        )
    }

    /// The choices saved in `dir`; none (the defaults) when the file is missing or unreadable.
    pub(crate) fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(FILE))
            .map_or_else(|_| Self::default(), |t| Self::parse(&t))
    }

    fn save(&self, dir: &Path) -> io::Result<()> {
        crate::renderer::save_file(dir, FILE, &self.text())
    }

    /// `Theme.glass`: whether the panes draw the real backdrop blur, on the GPU renderer unless
    /// transparency is reduced. Otherwise they draw the faked glass (ADR-0017 decision 3), solid
    /// under Reduce transparency through the theme's tokens.
    pub(crate) fn glass(&self, renderer: Renderer) -> bool {
        renderer == Renderer::OpenGl && !self.reduce_transparency
    }
}

/// Applies `appearance` to the window's theme. Call it before the first frame: a picked theme
/// then pins `StylePalette.color-scheme` from the start, so the system's never shows first.
pub(crate) fn apply(app: &App, appearance: &Appearance, renderer: Renderer) {
    let theme = app.global::<Theme>();
    if let Some(dark) = appearance.dark {
        theme.invoke_set_dark(dark);
    }
    theme.set_reduce_transparency(appearance.reduce_transparency);
    theme.set_reduce_motion(appearance.reduce_motion);
    theme.set_glass(appearance.glass(renderer));
}

/// Pins the native window's theme to the picked one (window.rs `set_window_theme`): the window is
/// frameless, so it only matters to the compositor and native dialogs. Nothing before the window
/// exists.
fn pin_window_theme(app: &App, dark: bool) {
    let theme = if dark {
        WindowTheme::Dark
    } else {
        WindowTheme::Light
    };
    app.window()
        .with_winit_window(|window| window.set_theme(Some(theme)));
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

/// Applies the choices saved in `config`, before the first frame, then wires the toggle (and
/// Ctrl+T) and the reduced-effects checkboxes, each saving every choice.
pub(crate) fn wire(app: &App, config: Option<PathBuf>, renderer: Renderer) {
    let saved = config.as_deref().map(Appearance::load).unwrap_or_default();
    apply(app, &saved, renderer);
    let current = Rc::new(Cell::new(saved));
    if let Some(dark) = saved.dark {
        // Once winit made the window (as main's `centre` waits for it).
        let window = app.as_weak();
        let pinned = slint::spawn_local(async move {
            let Some(app) = window.upgrade() else {
                return;
            };
            if app.window().winit_window().await.is_ok() {
                pin_window_theme(&app, dark);
            }
        });
        if let Err(error) = pinned {
            tracing::warn!("Cannot pin the window theme: {error}");
        }
    }

    let theme = app.global::<Theme>();
    theme.on_toggle({
        let (window, current, dir) = (app.as_weak(), Rc::clone(&current), config.clone());
        move || {
            let Some(app) = window.upgrade() else {
                return;
            };
            let theme = app.global::<Theme>();
            let dark = !theme.get_dark();
            theme.invoke_set_dark(dark);
            pin_window_theme(&app, dark);
            let appearance = Appearance {
                dark: Some(dark),
                ..current.get()
            };
            current.set(appearance);
            save(dir.as_deref(), &appearance);
        }
    });
    let window = app.as_weak();
    theme.on_effects_changed(move || {
        let Some(app) = window.upgrade() else {
            return;
        };
        let theme = app.global::<Theme>();
        let appearance = Appearance {
            reduce_transparency: theme.get_reduce_transparency(),
            reduce_motion: theme.get_reduce_motion(),
            ..current.get()
        };
        current.set(appearance);
        theme.set_glass(appearance.glass(renderer));
        save(config.as_deref(), &appearance);
    });
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
                reduce_transparency: true,
                reduce_motion: false,
            },
            Appearance {
                dark: Some(false),
                reduce_transparency: false,
                reduce_motion: true,
            },
            Appearance::default(),
        ] {
            appearance.save(&config).unwrap();
            assert_eq!(Appearance::load(&config), appearance);
        }
    }

    #[test]
    fn a_missing_or_corrupt_file_follows_the_system_with_every_effect() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Appearance::load(dir.path()), Appearance::default());
        // Not UTF-8: unreadable as text.
        std::fs::write(dir.path().join(FILE), [0xff, 0xfe, 0x00, 0x9f]).unwrap();
        assert_eq!(Appearance::load(dir.path()), Appearance::default());
        // Garbage, and values it cannot read, keep each default; a readable key still counts.
        std::fs::write(
            dir.path().join(FILE),
            "garbage\ntheme=purple\nreduce-transparency=yes\nreduce-motion=true\n",
        )
        .unwrap();
        assert_eq!(
            Appearance::load(dir.path()),
            Appearance {
                reduce_motion: true,
                ..Appearance::default()
            }
        );
    }

    #[test]
    fn the_glass_needs_the_gpu_and_full_transparency() {
        let reduced = Appearance {
            reduce_transparency: true,
            ..Appearance::default()
        };
        assert!(Appearance::default().glass(Renderer::OpenGl));
        assert!(!Appearance::default().glass(Renderer::Software));
        assert!(!reduced.glass(Renderer::OpenGl));
        assert!(!reduced.glass(Renderer::Software));
    }
}
