# desktop-slint/CLAUDE.md

The Slint host is a second view over the Desktop Core (ADR-0017). Before changing it, read `crates/mkv-desktop-core/CLAUDE.md` for the View State its `SlintPublisher` renders. Which capabilities and Intents the port has wired so far is tracked in `PARITY.md`.

The UI tests (`src/ui_tests.rs`) need a debug build: `ui/build.rs` keeps the element debug info that `find_by_element_type_name` searches only when `PROFILE` is `debug`, so under `cargo test --release` they fail.

The UI compiles with Slint's language edition 1.19, set only in `ui/build.rs` (`with_edition`); there is no `slint-project.json`. slint-compiler and slint-viewer use the 1.18 rules unless given `--edition 1.19`; slint-lsp has no `--edition` flag, so it and its live preview always use them (they would read a `slint-project.json`, which this repo does not add). Under the 1.18 rules they can lay out `if`/`for` children differently from the app.
