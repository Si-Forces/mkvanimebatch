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
| [x] | `pick_directory`: native folder dialog | `src/commands.rs:9`, `ui/app.js:619` | P2 | rfd (XDG portal, the Tauri host's features) on the runtime's blocking pool, then `ScanDirectory` (`intents.rs`). Not opened while busy. |
| [x] | `pick_subtitle_file`: native subtitle file dialog | `src/commands.rs:16`, `ui/app.js:560` | P3 | Same as above, with the subtitle filter (`inspector::SUBTITLE_FILTER`), one dialog at a time; not opened while busy, and a pick that answers after a batch started, or once the Inspector moved on to another row, is dropped. Cancel puts the select back on what is assigned. |
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
| [x] | `ScanDirectory` (path, force, no_cache) | `ui/directory-scan.js:14` | P2 | From the folder picker of the empty state and the failed-scan card; the Library's Open folder (brought into P2) and the input-dir field (P4) reuse `open-folder`. |
| [x] | `SetSelected` | `ui/queue-view.js:99` | P2 | The tick is drawn from the published row only, so a refused change has nothing to revert. |
| [x] | `SetAllSelected` | `ui/queue-view.js:521` | P2 | Select all / None. |
| [x] | `Reorder` (up/down) | `ui/queue-view.js:526` | P2 | |
| [x] | `Inspect` | `ui/queue-view.js:398` | P2 | Row highlighted at once (`pending-inspect`, cleared once answered). |
| [x] | `AssignSubtitle` | `ui/app.js:536` | P3 | Fields as typed, read when sent; the core builds the assignment. Sent when a choice or a language is picked (not the one shown), a file is picked, or a tag, title or offset is committed (Enter or focus out, once edited, as a webview `change`; a click on a control that takes no focus (a row, Open folder, a filter chip, a Library entry, a mode tab, Reset) commits it first, as the webview's blur does). Answered or refused, the fields are put back on what is published (`inspector::restore_form`), but a text field typed into since its last commit keeps its text on the same row (app.js redraws over it); a progress publication leaves typed fields alone (the `lastAssignKey` rule, `InspectorList`). |
| [x] | `ClearTrack` | `ui/app.js:545` | P3 | The Inspector's Reset, and Automatic in the select. |
| [ ] | `EditDraft` | `ui/settings-draft.js:85` | P4 | `false` answer restores the published value. |
| [ ] | `StepDraft` | `ui/settings-draft.js:90` | P4 | Stepper click; hold-to-repeat is the view's (400 ms, then 80 ms). |
| [ ] | `SetPreset` | `ui/preset-menu.js:62` | P4 | Remux only. |
| [ ] | `Run` | `ui/job-controller.js:74` | P5 | Run pending locks Run and the settings until answered. Every Run forces a scan, so a new `manifest_rev`, and `queue::diff_rows` then resets the whole row model where the webview redraws in place (same directory, same id order; `ui/queue-view.js:231-238`). Measure that reset in P5's bench; if it hitches, patch in place on the same directory and id sequence. |
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
| [ ] | Engine Diagnostics log file (`com.mkvanimebatch.desktop/logs/mkv-anime-batch.log`, 1 MB truncate, `RUST_LOG`, stderr) | `src/diagnostics.rs` | P0 | `desktop-slint/src/diagnostics.rs` is a port; until P9 deletes the Tauri copy, a fix goes into both. |
| [ ] | Transparent frameless window with rounded corners | `tauri.linux.conf.json` (`transparent: true`; Windows opaque) | P6 | The Slint window is transparent on every OS. Check Windows (DWM corner wedges); if wrong, set transparency per OS from the host. |
| [ ] | Test-only `FontProbe` exported from the UI crate | none (Slint only) | P9 | Ships in the binary because `.slint` has no cfg; drop it or move it to a test-only crate if the probe surface grows. |
| [ ] | OS detection classes (`is-windows`, `is-linux`, `is-mac`) for platform-specific styling | `ui/os-detect.js` | P6 | `cfg!` in the host if any styling still differs. |
| [ ] | Windows: no console window | `src/main.rs:2` (`windows_subsystem`) | P6 | Plus the in-window notice when the log file cannot be opened (ADR-0017 decision 9). |
| [ ] | GPU renderer failure fallback | none (webview) | P1 | OpenGL failure at startup: a dialog offers a restart in software mode and remembers it (`<config>/com.mkvanimebatch.desktop/renderer`). `MKV_RENDERER=software` forces it; `MKV_RENDERER=opengl` tries the GPU and, once it draws, forgets the saved choice. `Theme.glass` false, one warning (ADR-0017 renderer fallback amendment). |
| [ ] | Linux env workaround (`init_linux_env`, DMABUF, `GDK_BACKEND`, preload) | `src/lib.rs` | P9 | Dropped with Tauri (ADR-0017 Consequences); no counterpart. |

## Navigation, queue and scan

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [x] | Mode tabs Remux / Extract | `ui/index.html:65-79`, `ui/app.js:296` | P2 | The status pill is its own row (P5); the gear buttons come with the sheet (P4). One pane per mode, both kept (the hidden one invisible), so each list keeps its own scroll as the webview's two lists do; a card puts the list back at the top, as `clearList` does. |
| [ ] | Mode tab icons | `ui/index.html:65-79` | P5 | Not drawn yet. |
| [x] | Boot shows the extract view when a run or job is in extract | `ui/app.js:916` | P2 | `intents::boot_mode`. |
| [x] | List header: release title, crumbs path, `count_text` | `ui/index.html:105-106`, `ui/app.js:334` | P2 | `count_text` sits beside the title until P5 brings the bottom bar. Crumbs elide instead of wrapping. |
| [x] | Filter chips All / To do / Done / Failed with counts | `ui/index.html:109-112`, `ui/queue-view.js:430` | P2 | The click keeps the filter on the event loop, so the last click wins; the rows are mapped again off it. Accessible label "To do 3", as the webview chip's text. The chips, Select all and None wrap in a narrow list, as `.list-tools` does (P3, for the 760 px window with the Inspector), and so does the hint below them. |
| [x] | "No episodes match this filter." | `ui/index.html:131` | P2 | |
| [x] | Select all / None | `ui/index.html:115-116`, `ui/job-controller.js:123` | P2 | Locked while the selection is locked. |
| [x] | Rows: label, tone, title, meta, ring (status + progress), release group heads | `ui/queue-view.js:347` | P2 | Model reset only on `manifest_rev`/length/id change (`queue::diff_rows`). The status `title` is the accessible description. An unselected row reads `presentation::NOT_IN_BATCH`, the Inspector's const. |
| [ ] | Row status hover tooltip (`title`) | `ui/queue-view.js:347` | P6 | |
| [ ] | Sticky release group heads | `ui/queue-view.js:443` | P6 | The heads scroll with the rows. |
| [x] | Row checkbox with label "Include {name} in the batch" | `ui/queue-view.js:334` | P2 | Accessible label. |
| [x] | Row reorder buttons Move up / Move down | `ui/queue-view.js:356-357` | P2 | Shown on hover, hidden while a filter shows. |
| [x] | Row click inspects | `ui/queue-view.js:106` | P2 | |
| [x] | Empty state card with "Open folder…" | `ui/index.html:128`, `ui/queue-view.js:130` | P2 | Text is the published `empty`; the scanning and failed-scan cards too (`queue::map_head`). |
| [ ] | Folder picker from Browse… on input-dir fields | `ui/index.html:311,403`, `ui/app.js:617` | P2 | Picker on a worker, one at a time; busy is checked again when it answers; then `EditDraft { input_dir }` and the scan, as app.js does. The picker is P2's `open-folder`; the fields and their Browse… buttons come with the settings sheet (P4). |
| [x] | CJK filenames render | — | P2 | System font fallback: `ui_tests::cjk_names_render_through_the_system_font_fallback` checks it on the software renderer only (skips on a Linux machine with no CJK font, such as the CI runner); Skia is checked by eye. The first CJK glyph a process draws loads the fallback font in that frame (28 ms warm, 220 ms cold on the HD 4000 laptop), and each weight its own fallback candidates (600 and 700 open the bold ones), so `app.slint` draws a few at 400, 600 and 700, all but transparent, from the first frame on (a timer from the window's creation could run out before a slow cold first frame, and nothing was drawn). Parley prunes the candidates it does not draw with (`SourceCache::prune(128)` per layout), so the first CJK text shaped mid-run opens about 20 font files again (16 s into the bench); the pre-warm only makes those reads page-cache hits. The bench scrolls through Japanese and Simplified Chinese names. |

## Library and Episode Inspector

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [x] | Library tree; click reveals that release (filter back to All) | `ui/index.html:88`, `ui/app.js:377`, `ui/queue-view.js:418` | P3 | The sidebar scrolls, as `.side` does, so Open folder stays reachable past many releases. The folder's entry scrolls to the top, as the webview's does, without a filter change. A release's entry sets All, and once the rows are mapped again scrolls with `ListView::ensure-row-visible` from below every row, so the release's first row heads the list, measured rather than placed by the ListView's estimated row height; after a filter that hid every row the list has no row height to measure by, so the scroll waits for the layout that shows the rows, in the same frame (`ui_tests::the_library_reveals_a_release_at_the_top_of_its_list`). A filter clicked, the folder's entry or another folder after it cancels a reveal not shown yet. The entry's `title` tooltip is its accessible description; the hover tooltip comes with P6's. |
| [x] | "+ Open folder…" in the Library | `ui/index.html:89` | P2 | Brought forward from P3 so a loaded queue can open another folder; locked while a batch runs (`lock-while-job`). The tree (P3) goes above it. |
| [x] | Inspector empty text, number, title, path, error | `ui/index.html:166-179`, `ui/app.js:411` | P3 | The path, and the output folder's, wrap as `.crumbs` does (a `FlexboxLayout` of crumbs): a line breaks between crumbs, and a crumb wider than the line wraps at a space, else anywhere; the title and the error too. Columns follow `.panes` at each width (196/248 px below 980 px). |
| [x] | Inspector status, video, audio, output folder | `ui/index.html:184-187` | P3 | An unset output folder shows the output field's placeholder, per mode (`inspector::default_out`). |
| [x] | Subtitle ladder | `ui/index.html:193` | P3 | Each rung's accessible label names its number, source, what it found and its tag. |
| [x] | Subtitle assignment select, "Choose a file…" opens the subtitle picker, Reset | `ui/index.html:196-197`, `ui/app.js:551` | P3 | "Choose a file…" is a view text (`inspector::CHOOSE_FILE`). std-widgets `ComboBox` (fluent style), locked while busy. |
| [x] | Language select / custom tag / title / offset | `ui/index.html:199-215`, `ui/app.js:575` | P3 | Language labels are view texts (`inspector::LANGUAGES`). Picking "Custom tag…" focuses the tag field, which holds three characters at most (`maxlength`, `inspector::capped_tag`); the presets are checked against the core's `PRESET_SUBTITLE_LANGUAGES`. The offset is a text field; the core reads it as the number field's value was read (an unparsable offset is none). |
| [x] | Fonts chips | `ui/index.html:221-222` | P3 | |

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
| [ ] | Screen-reader labels (row checkbox, stepper buttons, title bar buttons, theme, complexity) | `ui/index.html`, `ui/queue-view.js:334` | P6 | AccessKit; one Orca pass at the P9 gate. The subtitle ladder is read as text nodes, one per rung, not as a list of list items (`ol`); the custom controls (filter chips, Select all / None, Library entries, mode tabs, Open folder, the row pick and checkbox) answer AccessKit's default action but take no keyboard focus: give them list semantics and Tab focus with the P6 focus rings. |
| [ ] | Keyboard focus reveals a row's Move up / Move down buttons | `ui/style.css:430` (`:focus-within`) | P6 | P2 reveals them on hover only (`queue.slint`); with the P6 focus rings. |

## Packaging and release

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | Product name, identifier `com.mkvanimebatch.desktop`, icons | `src-tauri/tauri.conf.json`, `src-tauri/icons/` | P8 | `.desktop` file and icon set committed. |
| [ ] | deb, rpm, AppImage | `.github/workflows/release.yml` | P8 | cargo-deb, cargo-generate-rpm, appimagetool. |
| [ ] | Windows exe + portable zip | `.github/workflows/release.yml` | P9 | From the switch release. |
| [ ] | Slint badge in README and release body | — | P8 | ADR-0017 decision 5. |
| [ ] | Binary renamed `mkv-anime-batch`, Tauri host deleted | — | P9 | |
| [ ] | Docs rewritten, CI drops WebKitGTK and tauri-cli | — | P10 | |
