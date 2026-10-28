# desktop-slint/CLAUDE.md

The Slint host is a second view over the Desktop Core (ADR-0017). Before changing it, read `crates/mkv-desktop-core/CLAUDE.md` for the View State its `SlintPublisher` renders. Which capabilities and Intents the port has wired so far is tracked in `PARITY.md`.

The UI tests (`src/ui_tests.rs`) need a debug build: `ui/build.rs` keeps the element debug info that `find_by_element_type_name` searches only when `PROFILE` is `debug`, so under `cargo test --release` they fail.
