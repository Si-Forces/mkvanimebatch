use tauri::{AppHandle, Manager};

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

fn parse_dir_for<T: serde::de::DeserializeOwned>(
    _f: impl FnOnce(&tauri::Window, T) -> tauri::Result<()>,
    dir_str: &str,
) -> Option<T> {
    let name = match dir_str {
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
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

#[tauri::command]
pub fn start_resize(window: tauri::Window, direction: String) {
    if let Some(dir) = parse_dir_for(tauri::Window::start_resize_dragging, &direction) {
        let _ = window.start_resize_dragging(dir);
    }
}

#[tauri::command]
pub fn set_window_theme(app: AppHandle, theme: String) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let tauri_theme = match theme.as_str() {
            "light" => tauri::Theme::Light,
            _ => tauri::Theme::Dark,
        };
        let _ = window.set_theme(Some(tauri_theme));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dir_for_all_valid_directions() {
        for dir in ["n", "s", "e", "w", "ne", "nw", "se", "sw"] {
            assert!(
                parse_dir_for(tauri::Window::start_resize_dragging, dir).is_some(),
                "Direction '{dir}' should parse successfully"
            );
        }
        assert!(parse_dir_for(tauri::Window::start_resize_dragging, "invalid").is_none());
        assert!(parse_dir_for(tauri::Window::start_resize_dragging, "").is_none());
    }
}
