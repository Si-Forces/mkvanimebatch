# Release Notes

<!-- scripts/release-body.sh, run by .github/workflows/release.yml, publishes each "## <version>" section on that release's page, without its "### Downloads" subsection: the page generates its own download table. A tag with no section here fails the release before it builds. -->

## 1.1.0

The desktop app's logic now runs in a Rust core inside the app ([ADR-0016](https://github.com/Si-Forces/mkvanimebatch/blob/main/docs/adr/0016-toolkit-agnostic-desktop-core.md)). The window only shows what the core publishes, which makes way for a native UI later. The files a batch writes are the same as in 1.0.1; the changes below are what you can see in the app.

### Improvements

- **Reloading the window** keeps the opened folder, the settings you typed, the run log and the episode open in the inspector, as well as a running batch. A reload loses only what the window alone holds, such as the open log drawer, the scroll position and the chosen filter.
- **Steppers** (Workers, Disk I/O, FFmpeg threads, Subtitle offset) now work from the keyboard: Enter or Space steps once, and holding Enter repeats ([#127](https://github.com/Si-Forces/mkvanimebatch/pull/127)).
- **A hidden window** stops updating the batch clock and the elapsed times, and refreshes them as soon as it is shown again.
- **Settings errors** for Workers, Disk I/O and FFmpeg threads name the field and the value, such as `Invalid workers value '2.5': must be a whole number.`

### Fixes

- A right or middle click on a stepper no longer changes its field ([#127](https://github.com/Si-Forces/mkvanimebatch/pull/127)).
- Closing the window while the first scan is still loading the inspection cache now waits up to 2 seconds for that scan to stop, instead of closing over it. If it has not stopped by then, the window stays open with a cleanup error.

### Behavior changes

- **Log colours and the error count**: a chapters or cover file that could not be moved into place during extract, and a `Warning:` line from MKVToolNix or about chapters, are now always yellow and no longer count toward the error badge, since the episode itself succeeded. A batch that crashes now logs a red `ERROR: Pipeline panicked: …` line that counts as an error.
- Setting the bitrate or complexity to the value it already has, or to a complexity that clamps to it, no longer switches the preset to Custom.
- Pressing Enter in the complexity box keeps the typed text until you leave the box, then shows the clamped value.
- An episode that failed with no error message reads "Failed · Failed" instead of "Failed · Error".

### Known issues

- **Windows testing**: 1.1.0 was tested by hand on Linux only. On Windows, the automated tests pass and the app builds, but it was not run by hand.
- **CPU use on Linux**: during a large batch, the window's rendering uses close to one CPU core even when nothing is scrolling ([#122](https://github.com/Si-Forces/mkvanimebatch/issues/122)).

## 1.0.1

A bug-fix release for 1.0.0.

### Fixes

- **Episode names**: a regular episode whose title contains `Special`, `SP`, `OVA`, `OAD`, or `ONA` is no longer treated as a special or an OVA. Remux wrote `Show - S01E07 - Special Service.mkv` as `SP00_Show - S01E07 - Special Service.mkv`, and extract named its subtitle and chapter files `…_SP00` instead of `…_EP07`. An explicit `SxxExx` or `SxxPxx` now decides the episode, and names without one are classified as before ([#104](https://github.com/Si-Forces/mkvanimebatch/pull/104)).
- **Desktop inspector**: the line under an episode title no longer repeats the file name. It shows only the episode folder, and is empty for an episode at the top of the opened folder ([#103](https://github.com/Si-Forces/mkvanimebatch/pull/103)).

### Behavior changes

- A file named like `Show - S00E03 - Special.mkv` is now season 0, episode 03, with no `SP` prefix. It used to get `SP00_`, the same prefix as every other `S00` file with a qualifier word. Its external subtitles, chapters, and covers now match that episode: a season-0 or season-less file such as `Show - S00E03.ass` or `Show - 03.ass`, never a season-1 one.
- A tie-in OVA named with both an OVA number and an `SxxExx`, such as `Show - OVA 03 (S01E05 tie-in).mkv`, is now season 1, episode 05 ([#105](https://github.com/Si-Forces/mkvanimebatch/issues/105)).

## 1.0.0

The first public release of MKV Anime Batch: a desktop app and a command-line tool (`mkv-cli`) that remux or extract a whole folder of anime fansub MKV episodes in one run. See the [README](../README.md) for installation and usage.

### Downloads

| Platform | Desktop app | CLI |
| :--- | :--- | :--- |
| Windows (x64) | `mkv-anime-batch.exe`, or `MKV_Anime_Batch_Portable_win64.zip` | `mkv-cli-x86_64-pc-windows-msvc.zip` |
| Linux (x86_64) | `MKV_Anime_Batch_1.0.0_amd64.AppImage`, `MKV_Anime_Batch_1.0.0_amd64.deb`, or `MKV_Anime_Batch-1.0.0-1.x86_64.rpm` | `mkv-cli-x86_64-unknown-linux-gnu.tar.gz` |

Each CLI archive has a `.sha256` file. There is no macOS build. `APPIMAGE-RUNTIME.txt` lists the GTK and WebKitGTK versions bundled in the AppImage.

You also need `mkvmerge` and `mkvextract` ([MKVToolNix](https://mkvtoolnix.download/)) and `ffmpeg` with `libopus` on your `PATH`. The Windows desktop app needs the Microsoft Edge WebView2 Runtime, which current Windows includes.

### Features

- **Remux**: one output MKV per episode with the chosen subtitle, one audio track, chapters, a cover, and the release's fonts. FLAC audio is transcoded to Opus with a preset (`balanced`, `fast`, `hifi`, `custom`) or your own bitrate and complexity. A global subtitle offset, or one per episode, shifts subtitle timing.
- **Extract**: the best ASS or SSA subtitle, chapters, cover, and fonts of each episode. Fonts are written once per fonts folder, deduplicated by name and size and by content hash.
- **Subtitle matching** across a release tree, in a fixed order: manual Track Assignment, same folder, embedded track, nearest parent folder, then `--assets-dir`. Episode identity is parsed from file names, including seasons, OVAs, specials, and fractional episodes.
- **Audio selection** prefers Japanese, then undetermined language, then stereo, and skips commentary and audio-description tracks.
- **Fast scanning**: a built-in Matroska parser reads track and attachment metadata without starting a process, with `mkvmerge -J` as the fallback. Results are cached by path, size, and modification time.
- **Concurrency**: several episodes at once (`--workers`), with a separate limit on disk-heavy steps (`--io-jobs`) so transcoding overlaps with muxing.
- **Safe output**: work is staged in a temporary folder inside the destination and moved into place when complete. Stopping a batch or a failure leaves no partial files.
- **Desktop app**: episode list grouped by release folder, a per-episode inspector with the subtitle ladder and Track Assignment controls, per-row progress, a log you can copy, CPU, RAM, and disk-write graphs, light and dark themes. A reloaded window reconnects to a running batch.
- **CLI**: `--plan` prints the inspection manifest as JSON, `--dry-run` shows what would run, `--events` streams job events as NDJSON, and `--selected-item`/`--selected-items` run chosen episodes in a given order.

### Known issues

- **AppImage rendering**: if the AppImage crashes at startup or shows a blank window, run it with `WEBKIT_DISABLE_DMABUF_RENDERER=1` and [open an issue](https://github.com/Si-Forces/mkvanimebatch/issues) with your GPU and driver. With the NVIDIA driver loaded the AppImage already sets this, so a crash there has another cause; report it the same way. Do not set it for the `.deb` or `.rpm` packages, where it breaks the window's rendering ([ADR-0015](https://github.com/Si-Forces/mkvanimebatch/blob/main/docs/adr/0015-per-package-webkitgtk-dmabuf-renderer-scoping.md)).
- **Windows testing**: the tests that run the real `mkvmerge`, `mkvextract`, and `ffmpeg` run only on Linux in CI. Windows builds pass the rest of the test suite.
- Stopping during a scan does not interrupt a file read already in progress. The batch stops, and that read finishes in the background.
- The desktop app and the CLI share the inspection cache file, but neither sees entries the other writes while it is running, and its next save drops them. The only cost is that those files are scanned again.

### Changes for users of pre-release builds

- `--keep-fonts` is removed: extract mode always keeps extracted fonts. `--sub-assignments` is removed: use `--assignments`, which still accepts the flat `{"video.mkv": "sub.ass"}` map.
- A selection that names any video that is not a discovered MKV now fails the whole run, instead of running the valid part. Two files that resolve to the same item ID also fail the run.
- A Track Assignment applies only to the video whose path relative to `--input-dir` matches `target_video`. A bare file name no longer matches the same name in a subfolder. `target_video` and `--selected-item` values are no longer trimmed.
- A candidate with the same known season as the video now beats one with no season, for subtitles, chapters, and episode covers in the same folder.
- A bitrate with a lowercase `m` suffix (`2m`) is rejected, because FFmpeg reads it as "milli". Use `M`.
- A subtitle extracted from a file whose name has no episode number is named `<stem>_<lang>_track<id>.ass`, without a repeated stem.
- The desktop Workers and Disk I/O fields start empty and use the engine's defaults, shown as `auto (N)`, instead of fixed values.
- The inspection cache file from earlier builds still loads.
