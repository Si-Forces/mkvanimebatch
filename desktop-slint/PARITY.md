# Slint port parity inventory

Every user-visible capability of the shipped Tauri app (v1.1.0, main `8fb2da3`), mapped to the
slice of the Slint port that delivers it (ADR-0017, #102). P9 (#142) ticks this list off before
the switch: the switch needs every row ticked or explicitly dropped.

Slices: P0 #133 foundation · P1 #134 renderer fallback · P2 #135 queue + scan · P3 #136 Library +
Inspector · P4 #137 settings · P5 #138 jobs + Run Log · P6 #139 telemetry, theme, glass, a11y,
Windows · P7 #140 Slint 1.19 · P8 #141 packaging · P9 #142 gate + switch · P10 #143 docs + CI
cleanup.

Paths: `ui/` is `desktop-tauri/ui/`, `src/` is `desktop-tauri/src-tauri/src/`, `actor.rs` is
`crates/mkv-desktop-core/src/actor.rs`.

## Tauri commands

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | `dispatch`: send an Intent, answer `bool` or the refusal | `src/commands.rs:37`, `ui/app.js:266` | P0 | Host calls `CoreHandle::dispatch` directly; it sends at call time (P0 core commit), so call order is Intent order and `UNCHAINED_INTENTS` has no equivalent to keep. |
| [ ] | `get_state`: whole View State | `src/commands.rs:25`, `ui/app.js:233` | P0 | No reload in a native host: the `Publisher` delivers every state; the coalescer keeps the newest `seq`. |
| [ ] | `get_log`: a mode's whole Run Log | `src/commands.rs:31`, `ui/app.js:910` | P5 | Read at boot into the log model. |
| [ ] | `pick_directory`: native folder dialog | `src/commands.rs:9`, `ui/app.js:619` | P2 | rfd (XDG portal), off the event loop; result sent back as an Intent. |
| [ ] | `pick_subtitle_file`: native subtitle file dialog | `src/commands.rs:16`, `ui/app.js:560` | P3 | Same as above, with the subtitle filter. |
| [ ] | `close_window`: two-phase teardown, then exit | `src/process.rs:29`, `ui/app.js:760` | P0 | Close request: `KeepWindowShown`, `begin_shutdown`, teardown on its own thread, then quit. |
| [ ] | `minimize_window` | `src/window.rs:2`, `ui/app.js:761` | P0 | Through `WinitWindowAccessor::set_minimized` (Wayland cache bug, ADR-0017 decision 4); repeated minimize must work. |
| [ ] | `toggle_maximize` | `src/window.rs:7`, `ui/app.js:762` | P0 | |
| [ ] | `is_maximized`: flatten the rounded corners when maximized | `src/window.rs:18`, `ui/app.js:745` | P0 | Bind the corner radius to the window's maximized state. |
| [ ] | `start_drag`: drag by the title bar, not its buttons | `src/window.rs:23`, `ui/app.js:770`, `ui/index.html:25` | P0 | `WindowMoveArea` behind the title bar's controls. |
| [ ] | `start_resize`: 8 edge/corner handles | `src/window.rs:46`, `ui/app.js:781`, `ui/index.html:15-22` | P0 | `resize-border-width`. |
| [ ] | `set_window_theme`: pin the native window theme to the chosen one | `src/window.rs:65`, `ui/theme.js:25` | P6 | Frameless, so it only matters for native dialogs and the compositor; set `StylePalette.color-scheme` and the winit theme. |

## Tauri events

| ✓ | Event | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | `view-state`: each publication, whole or as a patch | `src/publisher.rs:42`, `ui/app.js` (`nextState`) | P0 | `SlintPublisher`: no patch, the coalescer keeps the newest `seq`. |
| [ ] | `job-log`: each Run Log batch | `src/publisher.rs:50`, `ui/log-view.js` | P5 | `Publisher::logs` is a no-op until then. |
| [ ] | `job-done`: a job's end | `src/publisher.rs:54`, `ui/job-controller.js` | P5 | The coalescer already takes it after its state; nothing renders it yet. |
| [ ] | `cleanup-error` (`commands::CLEANUP_ERROR`): the close-failed notice | `src/lib.rs:132`, `ui/app.js:796` | P0 | Set from the Close thread through `upgrade_in_event_loop`. |

## Intents (`Intent` enum, `actor.rs:64`)

| ✓ | Intent | Sent from today | Slice | Notes |
|---|---|---|---|---|
| [ ] | `ScanDirectory` (path, force, no_cache) | `ui/directory-scan.js:14` | P2 | From the folder picker, the Library's Open folder, the empty state and the input-dir field. |
| [ ] | `SetSelected` | `ui/queue-view.js:99` | P2 | Revert the tick when the answer is `false`. |
| [ ] | `SetAllSelected` | `ui/queue-view.js:521` | P2 | Select all / None. |
| [ ] | `Reorder` (up/down) | `ui/queue-view.js:526` | P2 | |
| [ ] | `Inspect` | `ui/queue-view.js:398` | P2 | Row highlighted at once. |
| [ ] | `AssignSubtitle` | `ui/app.js:536` | P3 | Fields as typed; the core builds the assignment. |
| [ ] | `ClearTrack` | `ui/app.js:545` | P3 | The Inspector's Reset. |
| [ ] | `EditDraft` | `ui/settings-draft.js:85` | P4 | `false` answer restores the published value. |
| [ ] | `StepDraft` | `ui/settings-draft.js:90` | P4 | Stepper click; hold-to-repeat is the view's (400 ms, then 80 ms). |
| [ ] | `SetPreset` | `ui/preset-menu.js:62` | P4 | Remux only. |
| [ ] | `Run` | `ui/job-controller.js:74` | P5 | Run pending locks Run and the settings until answered. |
| [ ] | `Stop` | `ui/job-controller.js:96` | P5 | |
| [ ] | `ClearCache` | `ui/job-controller.js:110` | P5 | After the confirm dialog. |
| [ ] | `ViewVisible` | `ui/app.js:607` | P6 | From winit `Occluded` / minimize instead of `visibilitychange`. |

## Window and chrome

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | Frameless window 920×640, min 760×520, centred, title "MKV Anime Batch" | `src-tauri/tauri.conf.json` (`app.windows`) | P0 | |
| [ ] | Traffic-light close/minimize/maximize buttons with accessible labels | `ui/index.html:26-36` | P0 | Accessible labels "Close window", "Minimize window", "Maximize window". |
| [ ] | Traffic-light buttons reachable by Tab, Enter/Space activates, focus ring, tooltips (`title`) | `ui/index.html:26-36` | P6 | P0 buttons are pointer and AccessKit only. |
| [ ] | Double-click on the title bar toggles maximize | none (Slint only) | P0 | New in the Slint host, the native title-bar convention; drop it at P9 if unwanted. |
| [ ] | Brand mark and "MKV Anime Batch by Sifahaz" | `ui/index.html:37-50` | P0 | |
| [ ] | Closing exits cleanly; process cleanup error keeps the window open with a notice "Could not close safely" | `src/lib.rs:133-149`, `ui/app.js:732`, `ui/app.js:796` | P0 | `cancel_shutdown` on error, then show it. Tauri shows a modal with a confirm button (`showModal`); P0 shows a banner in the window that the next Close clears. |
| [ ] | Engine Diagnostics log file (`com.mkvanimebatch.desktop/logs/mkv-anime-batch.log`, 1 MB truncate, `RUST_LOG`, stderr) | `src/diagnostics.rs` | P0 | |
| [ ] | OS detection classes (`is-windows`, `is-linux`, `is-mac`) for platform-specific styling | `ui/os-detect.js` | P6 | `cfg!` in the host if any styling still differs. |
| [ ] | Windows: no console window | `src/main.rs:2` (`windows_subsystem`) | P6 | Plus the in-window notice when the log file cannot be opened (ADR-0017 decision 9). |
| [ ] | GPU renderer failure fallback | none (webview) | P1 | Software renderer, faked glass, one warning. |
| [ ] | Linux env workaround (`init_linux_env`, DMABUF, `GDK_BACKEND`, preload) | `src/lib.rs` | P9 | Dropped with Tauri (ADR-0017 Consequences); no counterpart. |

## Navigation, queue and scan

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | Mode tabs Remux / Extract with status pill | `ui/index.html:65-79`, `ui/app.js:296` | P2 | The pill's text and tone are P5 (published `controls`). |
| [ ] | Boot shows the extract view when a run or job is in extract | `ui/app.js:916` | P2 | |
| [ ] | List header: release title, crumbs path, `count_text` | `ui/index.html:105-106`, `ui/app.js:334` | P2 | P0 binds title, `count_text` and `busy` to prove the Publisher path. |
| [ ] | Filter chips All / To do / Done / Failed with counts | `ui/index.html:109-112`, `ui/queue-view.js:430` | P2 | |
| [ ] | "No episodes match this filter." | `ui/index.html:131` | P2 | |
| [ ] | Select all / None | `ui/index.html:115-116`, `ui/job-controller.js:123` | P2 | Locked while the selection is locked. |
| [ ] | Rows: label, tone, title, meta, ring (status + progress), release group heads | `ui/queue-view.js:347` | P2 | Model reset only on `manifest_rev`/length/id change. |
| [ ] | Row checkbox with label "Include {name} in the batch" | `ui/queue-view.js:334` | P2 | Accessible label. |
| [ ] | Row reorder buttons Move up / Move down | `ui/queue-view.js:356-357` | P2 | |
| [ ] | Row click inspects | `ui/queue-view.js:106` | P2 | |
| [ ] | Empty state card with "Open folder…" | `ui/index.html:128`, `ui/queue-view.js:130` | P2 | Text is the published `empty`. |
| [ ] | Folder picker from Browse… on input-dir fields | `ui/index.html:311,403`, `ui/app.js:617` | P2 | Picker on a worker; scans after. |
| [ ] | CJK filenames render | — | P2 | Font fallback. |

## Library and Episode Inspector

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | Library tree; click reveals that release (filter back to All) | `ui/index.html:88`, `ui/app.js:377`, `ui/queue-view.js:418` | P3 | |
| [ ] | "+ Open folder…" in the Library | `ui/index.html:89` | P3 | Uses P2's picker. |
| [ ] | Inspector empty text, number, title, path, error | `ui/index.html:166-179`, `ui/app.js:411` | P3 | |
| [ ] | Inspector status, video, audio, output folder | `ui/index.html:184-187` | P3 | |
| [ ] | Subtitle ladder | `ui/index.html:193` | P3 | |
| [ ] | Subtitle assignment select, "Choose a file…" opens the subtitle picker, Reset | `ui/index.html:196-197`, `ui/app.js:551` | P3 | "Choose a file…" is a view text. |
| [ ] | Language select / custom tag / title / offset | `ui/index.html:199-215`, `ui/app.js:575` | P3 | Language labels are view texts. |
| [ ] | Fonts chips | `ui/index.html:221-222` | P3 | |

## Settings

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | Settings sheet per mode (gear button), Done, backdrop click / Esc closes, focus back to opener | `ui/index.html:70,80,291-298`, `ui/app.js:682,817-833` | P4 | |
| [ ] | Remux fields: input/out/fonts/assets dirs with Browse…, bitrate, subtitle offset, complexity slider + value, workers, I/O jobs, FFmpeg threads, dry run, no cache | `ui/index.html:304-391` | P4 | Browse… for non-input dirs sends `EditDraft`. |
| [ ] | Extract fields: input/out/fonts dirs with Browse…, workers, I/O jobs, dry run, no cache | `ui/index.html:395-444` | P4 | |
| [ ] | Draft write-back into unfocused fields; complexity restored on blur | `ui/settings-draft.js:114-116` | P4 | |
| [ ] | Input-dir field change/blur rescans | `ui/job-controller.js:51-52` | P4 | With P2's scan. |
| [ ] | Steppers −/+ with hold-to-repeat and keyboard activation | `ui/app.js:98-125` | P4 | |
| [ ] | Preset menu (remux) | `ui/index.html:92`, `ui/preset-menu.js` | P4 | |
| [ ] | Worker / I/O chips and `auto (N)` hints | `ui/settings-chips.js` | P4 | |
| [ ] | Flagged field (`input-error`) and the sheet opened on it after a refused Run | `ui/job-controller.js:178` | P4 | |

## Jobs and Run Log

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | Run remux / Run extract (label, tooltip), Stop | `ui/index.html:232-237`, `ui/job-controller.js` | P5 | |
| [ ] | Queue count, clock, progress stack (done/skip/fail/live) | `ui/index.html:241-246` | P5 | |
| [ ] | Status pill per mode | `ui/index.html:68,78` | P5 | |
| [ ] | "Clear inspection cache…" with confirm dialog (Cancel / Clear cache, Esc cancels) | `ui/index.html:391,444,452-458`, `ui/job-controller.js:102` | P5 | Dialogs queue: a second waits for the first. |
| [ ] | Log drawer toggle with error badge | `ui/index.html:269-276`, `ui/app.js:642-657` | P5 | Glass is P6. |
| [ ] | Per-mode console: generation clears, `keep_from` trims, live region | `ui/log-view.js`, `ui/index.html:284-285` | P5 | |
| [ ] | Copy (raw text with real paths) to the clipboard; "Copied" / "Copy failed" for 1.5 s | `ui/app.js:659-672` | P5 | `arboard` or Slint clipboard. |
| [ ] | Close button in the drawer | `ui/index.html:281` | P5 | |
| [ ] | Esc closes the drawer unless a dialog is open | `ui/app.js:880` | P5 | |

## Telemetry, theme, glass, accessibility

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | CPU / RAM / Disk write values and sparklines | `ui/index.html:256-265`, `ui/app.js:591`, `ui/presentation.js` | P6 | `sparkPoints` geometry moves to Rust. |
| [ ] | Theme toggle button | `ui/index.html:52` | P6 | |
| [ ] | Ctrl+T (Cmd+T) toggles the theme | `ui/app.js:875` | P6 | |
| [ ] | Theme follows the OS live until the user picks one | `ui/theme.js:43` | P6 | `StylePalette.color-scheme`. |
| [ ] | Saved theme choice (`mkv-theme` localStorage key), applied before first paint | `ui/theme-init.js:9`, `ui/theme.js:57` | P6 | Saved in a file under `dirs::config_dir()/com.mkvanimebatch.desktop/` (Q96); the webview key is not migrated (switch release notes say so). |
| [ ] | `prefers-reduced-transparency`: solid panes | `ui/style.css:96` | P6 | In-app "Reduce transparency" setting, saved with the theme (Q105). |
| [ ] | `prefers-reduced-motion`: no animations | `ui/style.css:243,648` | P6 | In-app "Reduce motion" setting (Q105). |
| [ ] | Glass log drawer | `ui/style.css` (`.glass`) | P6 | `backdrop-blur` (Slint 1.19, P7 for the release pin). |
| [ ] | Fonts Onest, Unbounded, JetBrains Mono | `ui/fonts/*.woff2` | P0 | Variable TTFs embedded; OFL texts beside them. |
| [ ] | Screen-reader labels (row checkbox, stepper buttons, title bar buttons, theme, complexity) | `ui/index.html`, `ui/queue-view.js:334` | P6 | AccessKit; one Orca pass at the P9 gate. |

## Packaging and release

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | Product name, identifier `com.mkvanimebatch.desktop`, icons | `src-tauri/tauri.conf.json`, `src-tauri/icons/` | P8 | `.desktop` file and icon set committed. |
| [ ] | deb, rpm, AppImage | `.github/workflows/release.yml` | P8 | cargo-deb, cargo-generate-rpm, appimagetool. |
| [ ] | Windows exe + portable zip | `.github/workflows/release.yml` | P9 | From the switch release. |
| [ ] | Slint badge in README and release body | — | P8 | ADR-0017 decision 5. |
| [ ] | Binary renamed `mkv-anime-batch`, Tauri host deleted | — | P9 | |
| [ ] | Docs rewritten, CI drops WebKitGTK and tauri-cli | — | P10 | |
