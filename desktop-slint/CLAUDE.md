# desktop-slint/CLAUDE.md

The Slint host is a second view over the Desktop Core (ADR-0017). Before changing it, read `crates/mkv-desktop-core/CLAUDE.md` for the View State its `SlintPublisher` renders. Which capabilities and Intents the port has wired so far is tracked in `PARITY.md`.

The UI tests install the Slint testing platform once per thread, so they rely on libtest running each test on a thread of its own: `--test-threads=1` makes every test after the first panic (`SetPlatformError::AlreadySet`).
