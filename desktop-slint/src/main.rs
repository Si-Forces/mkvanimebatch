//! MKV Anime Batch desktop host on Slint (ADR-0017), beside the Tauri host until the switch.

use mkv_anime_batch_slint_ui::App;
use slint::ComponentHandle;

fn main() -> Result<(), slint::PlatformError> {
    App::new()?.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_window_opens_headless() {
        i_slint_backend_testing::init_no_event_loop();
        let app = App::new().expect("create the window");
        app.show().expect("show the window");
        app.hide().expect("hide the window");
    }
}
