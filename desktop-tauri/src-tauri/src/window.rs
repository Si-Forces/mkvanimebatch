#[tauri::command]
pub fn minimize_window(window: tauri::Window) {
    let _ = window.minimize();
}

#[tauri::command]
pub fn toggle_maximize(window: tauri::Window) {
    if let Ok(maximized) = window.is_maximized() {
        if maximized {
            let _ = window.unmaximize();
        } else {
            let _ = window.maximize();
        }
    }
}

#[tauri::command]
pub fn is_maximized(window: tauri::Window) -> bool {
    window.is_maximized().unwrap_or(false)
}

#[tauri::command]
pub fn start_drag(window: tauri::Window) {
    let _ = window.start_dragging();
}

/// Maps a resize-handle code (`n`, `se`, ...) to Tauri's resize direction.
/// Tauri does not re-export `ResizeDirection`, so the value is built through its
/// `Deserialize` impl and the type is inferred from `start_resize_dragging`.
fn resize_direction<D: serde::de::DeserializeOwned>(code: &str) -> Option<D> {
    let name = match code {
        "n" => "North",
        "s" => "South",
        "e" => "East",
        "w" => "West",
        "ne" => "NorthEast",
        "nw" => "NorthWest",
        "se" => "SouthEast",
        "sw" => "SouthWest",
        _ => return None,
    };
    serde_json::from_value(name.into()).ok()
}

#[tauri::command]
pub fn start_resize(window: tauri::Window, direction: String) {
    if let Some(direction) = resize_direction(&direction) {
        let _ = window.start_resize_dragging(direction);
    }
}

#[tauri::command]
pub fn set_window_theme(window: tauri::Window, theme: String) {
    let theme = match theme.as_str() {
        "light" => tauri::Theme::Light,
        _ => tauri::Theme::Dark,
    };
    let _ = window.set_theme(Some(theme));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins `D` to the parameter type of `start_resize_dragging`.
    fn direction_for<D: serde::de::DeserializeOwned>(
        _resize: impl FnOnce(&tauri::Window, D) -> tauri::Result<()>,
        code: &str,
    ) -> Option<D> {
        resize_direction(code)
    }

    #[test]
    fn resize_codes_name_real_tauri_directions() {
        for code in ["n", "s", "e", "w", "ne", "nw", "se", "sw"] {
            assert!(
                direction_for(tauri::Window::start_resize_dragging, code).is_some(),
                "resize code '{code}' must map to a ResizeDirection"
            );
        }
        assert!(direction_for(tauri::Window::start_resize_dragging, "invalid").is_none());
    }
}
