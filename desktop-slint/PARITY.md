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
| [x] | `get_log`: a mode's whole Run Log | `src/commands.rs:31`, `ui/app.js:910` | P5 | Read once at boot (`run_log::load`, spawned from `main`): both modes asked at call time, so a batch published before the answer is in it and one after waits in the console until the answer is drawn, with no line drawn twice (`run_log::tests::get_log_at_boot_reads_the_run_log_into_each_console`, through the real core). |
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
| [x] | `job-log`: each Run Log batch | `src/publisher.rs:50`, `ui/log-view.js` | P5 | `Publisher::logs` draws the batch in its mode's console (`run_log::Console`, log-view.js's `draw`) under the coalescer's lock, off the UI thread, and the hop writes the resulting patch (clear, drop the oldest rows, append) into the mode's `VecModel<LogRow>`. When the event loop is gone a failed hop clears the scheduled flag and drops the pending dones; the rest, bounded, stays (a test's `flush` writes it). |
| [x] | `job-done`: a job's end | `src/publisher.rs:54`, `ui/job-controller.js` | P5 | The coalescer takes it after its state; nothing renders it, as no webview script listens to it: the job's end shows through the View State (the pill, the clock, Run back). |
| [x] | `cleanup-error` (`commands::CLEANUP_ERROR`): the close-failed notice | `src/lib.rs:132`, `ui/app.js:796` | P0, P5 | Set from the Close thread through `upgrade_in_event_loop`, which invokes `close-failed`; P5 shows it as `showCleanupError` does: an OK-only dialog, queued with the others (`jobs::close_failed_dialog`). |

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
| [x] | `EditDraft` | `ui/settings-draft.js:85` | P4 | Sent as typed on each keystroke, again on Enter or when the field is left once edited (a `change`), on a checkbox's toggle and twice on a bitrate pick (input, change), as the webview does; the slider sends its whole value and the box follows it. Guarded per key until answered (`settings::PendingEdits`): a refusal puts the published value back (into the focused field too), a `false` does only for the complexity (`settings::settle`), and so does any answer once a publication passed over a value the core moved the key to while it was pending (no later publication may come to show it). |
| [x] | `StepDraft` | `ui/settings-draft.js:90` | P4 | Stepper press; hold-to-repeat is the host's `settings::Repeater` (400 ms, then 80 ms; tested on mock time). Guarded like `EditDraft`; nothing while a batch runs. |
| [x] | `SetPreset` | `ui/preset-menu.js:62` | P4 | Remux only; not sent while a batch runs. |
| [x] | `Run` | `ui/job-controller.js:74` | P5 | Run pending (`run-pending`, the host's) locks Run and, through `locked`, every control a batch locks until answered (`jobs::run_answered`). Every Run forces a scan, so a new `manifest_rev`, and `queue::diff_rows` then resets the whole row model where the webview redraws in place (same directory, same id order; `ui/queue-view.js:231-238`). Measured in the queue bench (`run_reset_ms`, `run_reset_frame`; Wayland, Skia GL, 1000 rows, 2026-10-25): the reset hop takes 1.1–1.2 ms on the UI thread (budget 4 ms), and the frame it lands in renders in 8–9.5 ms, with a 21–23 ms interval (no missed vsync), so the reset stays; patch in place on the same directory and id sequence if a later bench shows it hitch. The bench reapplies the scroll offset every frame, so whether a Run keeps the list's scroll is a manual check. |
| [x] | `Stop` | `ui/job-controller.js:96` | P5 | The bar's Stop while the mode shown owns the batch. |
| [x] | `ClearCache` | `ui/job-controller.js:110` | P5 | After the confirm dialog, for the sheet's mode. |
| [ ] | `ViewVisible` | `ui/app.js:607` | P6 | From winit `Occluded` / minimize instead of `visibilitychange`. |

## Window and chrome

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [ ] | Frameless window 920×640, min 760×520, centred, title "MKV Anime Batch" | `src-tauri/tauri.conf.json` (`app.windows`) | P0 | |
| [ ] | Traffic-light close/minimize/maximize buttons with accessible labels | `ui/index.html:26-36` | P0 | Accessible labels "Close window", "Minimize window", "Maximize window". |
| [ ] | Traffic-light buttons reachable by Tab, Enter/Space activates, focus ring, tooltips (`title`) | `ui/index.html:26-36` | P6 | P0 buttons are pointer and AccessKit only. |
| [ ] | Double-click on the title bar toggles maximize | none (Slint only) | P0 | New in the Slint host, the native title-bar convention; drop it at P9 if unwanted. |
| [ ] | Brand mark and "MKV Anime Batch by Sifahaz" | `ui/index.html:37-50` | P0 | |
| [ ] | Closing exits cleanly; process cleanup error keeps the window open with a notice "Could not close safely" | `src/lib.rs:133-149`, `ui/app.js:732`, `ui/app.js:796` | P0, P5 | `cancel_shutdown` on error, then show it. As Tauri's modal (`showModal`, OK alone): P0's banner became that dialog in P5 (#138), queued behind a dialog already shown. |
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
| [x] | Mode tabs Remux / Extract | `ui/index.html:65-79`, `ui/app.js:296` | P2 | The status pill is its own row (P5); each tab's gear opens its settings sheet (P4). One pane per mode, both kept (the hidden one invisible), so each list keeps its own scroll as the webview's two lists do; a card puts the list back at the top, as `clearList` does. |
| [x] | Mode tab icons | `ui/index.html:65-79` | P5 | `Path`s of the SVGs; the stroke scaled to 16 px by hand (a `Path`'s stroke is not scaled with its viewbox). With the icon and the pill the tab's texts elide at 196 px, where the webview's wrap; an idle tab makes no dot, so it takes no room, as `.status-idle { display: none }`. |
| [x] | Boot shows the extract view when a run or job is in extract | `ui/app.js:916` | P2 | `intents::boot_mode`. |
| [x] | List header: release title, crumbs path, `count_text` | `ui/index.html:105-106`, `ui/app.js:334` | P2 | `count_text` is in the bottom bar (P5). Crumbs elide instead of wrapping. |
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
| [x] | Folder picker from Browse… on input-dir fields | `ui/index.html:311,403`, `ui/app.js:617` | P2 | Picker on a worker, one at a time (Browse… and Open folder share it, `settings::browse`); busy is checked again when it answers, and the folder goes to the field and mode that opened it: written into the field, then `EditDraft` (guarded) and, for the input folder, the scan, as `chooseInputDirectory` does. |
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
| [x] | Settings sheet per mode (gear button), Done, backdrop click / Esc closes, focus back to opener | `ui/index.html:70,80,291-298`, `ui/app.js:682,817-833` | P4 | `settings.slint` `SettingsSheet` over a backdrop; opens with Done focused, as it does for a key it shows no field of; `open-sheet` leaves the focused field under its own mode before it switches; a closing sheet leaves the focused field first (it changes if edited, as a closing dialog blurs it). The gear takes the focus back (Enter or Space opens it again); its focus ring, and the webview's quiet focus after a pointer close, come with P6's rings. Modal for the keyboard: Tab and Shift+Tab wrap between Done and the last control, so the focus never reaches the window behind and Esc keeps closing it. A focused control (Tab, or the flagged field) scrolls into view. The sheet is near opaque with the `.glass` edge and shadow, since it has no blur behind it. Fits 760x520: the body scrolls. A sheet the host opens (`open-sheet`, after a refused Run) gives the focus back to Run. |
| [x] | Remux fields: input/out/fonts/assets dirs with Browse…, bitrate, subtitle offset, complexity slider + value, workers, I/O jobs, FFmpeg threads, dry run, no cache | `ui/index.html:304-391` | P4 | Browse… for non-input dirs sends `EditDraft`. The placeholders are drawn by `DraftEdit` in `Theme.hint` (the fluent LineEdit's own is near white in the dark theme and its colour cannot be set), in the field's font as the browser's `::placeholder` inherits it, and hidden while the field has the focus, where an input method's preedit would draw over it; the webview keeps showing it until a key is typed. The bitrate entries are the host's (`settings::BITRATES`). "Clear inspection cache…" is P5's, with its confirm dialog. Tooltips are the fields' accessible descriptions, their ranges and the slider's from the core's (`settings::limits`). Workers, Disk I/O, FFmpeg threads and the complexity box take digits alone (`InputType.number`, as `type=number`); the subtitle offset takes any text, since a decimal field follows the locale's separator while the core reads a point. |
| [x] | Extract fields: input/out/fonts dirs with Browse…, workers, I/O jobs, dry run, no cache | `ui/index.html:395-444` | P4 | With the extract note. |
| [x] | Draft write-back into unfocused fields; complexity restored on blur | `ui/settings-draft.js:114-116` | P4 | `settings::show` in the hop: every key but the focused field's (`Sheet.focused`) and the pending ones. The slider (focus id `complexity_slider`) and the box both edit the `complexity` key, so a focused slider spares the box too; their own focus ids keep a focus moving between them from clearing it. A complexity left gets the published value back unless its edit is still pending. |
| [x] | Input-dir field change/blur rescans | `ui/job-controller.js:51-52` | P4 | With P2's scan, of the trimmed path: on a change (Enter, or left once edited) and on every blur that did not just change it (the webview scans twice then). |
| [x] | Steppers −/+ with hold-to-repeat and keyboard activation | `ui/app.js:98-125` | P4 | A left press steps and repeats until release or the pointer leaves; the button takes the focus; Enter, Space or the screen reader's action steps once. A sheet closed under a held button, or a batch starting, stops its repeat. |
| [x] | Preset menu (remux) | `ui/index.html:92`, `ui/preset-menu.js` | P4 | In the sidebar under the Library, while remux shows; "Locked", with its lock, in its heading while a batch runs. Row texts from `settings::map_presets`; a complexity that is not a number yet keeps the custom row's subtitle. |
| [x] | Worker / I/O chips and `auto (N)` hints | `ui/settings-chips.js` | P4 | The chips sit in the bottom bar from 900 px, as `.bar-chips` does; the bar shows at every width (P5). The hints are the Workers and Disk I/O placeholders (`settings::map_chips`), `auto` until known. The chips' `title` is a plain-text `Tooltip` and their accessible description (P5). |
| [x] | Flagged field (`input-error`) and the sheet opened on it after a refused Run | `ui/job-controller.js:178` | P4 | An error ring on the flagged field (both complexity controls for `complexity`). `settings::open_on_flagged` opens the sheet on it (the cursor in the box), scrolled into view, tested directly at 760x520; Run calls it once refused (P5). |

## Jobs and Run Log

| ✓ | What | Where today | Slice | Notes |
|---|---|---|---|---|
| [x] | Run remux / Run extract (label, tooltip), Stop | `ui/index.html:232-237`, `ui/job-controller.js` | P5 | One button in the bar (`BottomBar`): the mode's Run, or its Stop while it owns the batch, as the webview hides one of the two. The tooltip is a `Tooltip` with plain text (its own `text` is markdown), also the accessible description. A refused Run calls `settings::open_on_flagged` (P4), with Run as the sheet's opener, so the focus goes back to it, on the event loop (`upgrade_in_event_loop` from the answer) and only for a draft-validation refusal: `RUN_BUSY` and `RUN_SCANNING` answer before the core takes `invalid`, so a previous refusal's flag is still published (job-controller.js does not tell the refusal kinds apart; it relies on Run being locked while busy or scanning). The core exports both texts (`actor::RUN_BUSY`, `RUN_SCANNING`) for that, and its `start.rs` tests pin them as `run_batch`'s answers; a typed refusal would change the Tauri contract's `Result<bool, String>`, so it waits for a third kind. Tested through the real `Intent::Run` answer (`jobs::tests`). Run, Stop, the Log toggle, Copy, Close and the pill buttons take the focus from a left press, as `StepButton` does (the `TouchArea` over their `FocusScope` takes the press first), so Return then activates the one clicked; their ring shows for the pointer until P6 quiets it. The Run and the cache dialog went into `jobs.rs`, so `settings.rs` did not grow and is not split. |
| [x] | Queue count, clock, progress stack (done/skip/fail/live) | `ui/index.html:241-246` | P5 | The mode shown's, from its `controls`, `count_text`, `clock` and `progress` (`jobs::map_controls`). |
| [x] | Status pill per mode | `ui/index.html:68,78` | P5 | A dot in the tab, none while idle; its text is the tab's accessible description, as the webview's tab reads it out. The subtitle wraps onto a second line when the tab is narrow (the dot shown, or a 196 px sidebar), and the tab grows past 44 px for it, as `.nav-text small` does. |
| [x] | "Clear inspection cache…" with confirm dialog (Cancel / Clear cache, Esc cancels) | `ui/index.html:391,444,452-458`, `ui/job-controller.js:102` | P5 | Dialogs queue: a second waits for the first (`jobs::Dialogs`). Cancel takes the focus as it shows, Tab stays on its two buttons, and the sheet's button has the focus back once it closes. The Close-failed notice is a dialog of its own (OK alone, `DialogTexts::notice`), queued the same way; a text too long for the window elides, so OK stays in it (the whole text is the card's accessible description). A dialog closed with no sheet open gives the focus to the window's key scope, so Esc still closes the drawer; the webview's `<dialog>` gives it back to the element that had it. |
| [x] | Log drawer toggle with error badge | `ui/index.html:269-276`, `ui/app.js:642-657` | P5 | `LogToggle` at the end of the bar (the telemetry, P6, goes before it): focusable, `accessible-expanded` for `aria-expanded`, the badge the shown mode's error count as the core counts it, its text the host's (`run_log::badge`), drawn in `err-fg` on `err` (AA; style.css's white on dark `--err` is not). The toggle's accessible description is the count spoken ("1 error", "3 errors"; `run_log::badge`). The drawer is a near opaque `float` pane with the `.glass` edge and shadow; the glass is P6. |
| [x] | Per-mode console: generation clears, `keep_from` trims, live region | `ui/log-view.js`, `ui/index.html:284-285` | P5 | One virtualised `ListView` per mode, both kept (one shown), each line in its kind's tone with the core's display text, wrapped (word-wrap breaks a long path anywhere, as `overflow-wrap: anywhere`). It follows the newest line while scrolled to within 32 px of the bottom, as `flush` does, with `ensure-row-visible` so the row is measured; content that fits (a new generation emptied it) follows again. The empty prompt shows while a console holds no line. `accessible-live-region: polite` on the list, Slint's `aria-live` (AccessKit `Live::Polite`); not checked with a screen reader yet (P6). The drawer is made from the start and hidden while closed, so a console follows while closed too (`run_log::tests::a_closed_drawer_opens_on_the_newest_line`). |
| [x] | Copy (raw text with real paths) to the clipboard; "Copied" / "Copy failed" for 1.5 s | `ui/app.js:659-672` | P5 | `arboard`, already in the tree through Slint's winit backend (its features, Wayland data-control on Linux, come from there): Slint 45cb2b3 has no app-level clipboard call, and its backends drop the write's error, so "Copy failed" could never show. Opened at the first Copy and kept, as X11 serves the text from its owner. A second Copy within 1.5 s restarts the wait (one `Timer`), where the webview's earlier timeout resets the label early. Tested on mock time with a fake clipboard. |
| [x] | Close button in the drawer | `ui/index.html:281` | P5 | Copy and Close take the focus, from the keyboard or a press (the queue's link buttons do not); closing while either has it hands it to the Log toggle, as `setLogOpen` does. |
| [x] | Esc closes the drawer unless a dialog is open | `ui/app.js:880` | P5 | A `FocusScope` around the window's content (not the sheet or the dialog, which take Esc themselves) closes it, unless the sheet or the confirm dialog is open (both `<dialog>`s in the webview). Slint drops a key nothing has the focus for, so that scope takes the focus at start, as the webview's `document` listener hears every key; an item hidden while focused still drops the focus. |

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
| [ ] | Screen-reader labels (row checkbox, stepper buttons, title bar buttons, theme, complexity) | `ui/index.html`, `ui/queue-view.js:334` | P6 | AccessKit; one Orca pass at the P9 gate. The subtitle ladder is read as text nodes, one per rung, not as a list of list items (`ol`); the custom controls (filter chips, Select all / None, Library entries, mode tabs, Open folder, the row pick and checkbox) answer AccessKit's default action but take no keyboard focus: give them list semantics and Tab focus with the P6 focus rings. Revisit the window-wide `FocusScope` that holds the focus from the start so Esc closes the Log drawer (P5): kept for now; with Tab focus on every control, a key may always have a focused item to bubble from. |
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
