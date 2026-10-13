# Release Notes

## 1.0.0

The first public release of MKV Anime Batch: a desktop app and a command-line tool (`mkv-cli`) that remux or extract a whole folder of anime fansub MKV episodes in one run. See the [README](../README.md) for installation and usage.

### Downloads

| Platform | Desktop app | CLI |
| :--- | :--- | :--- |
| Windows (x64) | `mkv-anime-batch.exe`, or `MKV_Anime_Batch_Portable_win64.zip` | `mkv-cli-x86_64-pc-windows-msvc.zip` |
| Linux (x86_64) | `MKV_Anime_Batch_*_amd64.AppImage`, `MKV_Anime_Batch_*_amd64.deb`, or `MKV_Anime_Batch-*.x86_64.rpm` | `mkv-cli-x86_64-unknown-linux-gnu.tar.gz` |

Each CLI archive has a `.sha256` file. `APPIMAGE-RUNTIME.txt` lists the GTK and WebKitGTK versions bundled in the AppImage.

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

- **AppImage rendering**: if the AppImage crashes at startup or shows a blank window, run it with `WEBKIT_DISABLE_DMABUF_RENDERER=1` and [open an issue](https://github.com/Si-Forces/mkvanimebatch/issues) with your GPU and driver. With the NVIDIA driver loaded the AppImage already sets this. Do not set it for the `.deb` or `.rpm` packages, where it breaks the window's rendering ([ADR-0015](adr/0015-per-package-webkitgtk-dmabuf-renderer-scoping.md)).
- **Windows testing**: the tests that run the real `mkvmerge`, `mkvextract`, and `ffmpeg` run only on Linux in CI. Windows builds pass the rest of the test suite.
- **No macOS build.**
- Stopping during a scan does not interrupt a file read already in progress. The batch stops, and that read finishes in the background.
- The desktop app and the CLI share the inspection cache file, but neither sees entries the other writes while it is running, and its next save drops them. The only cost is that those files are scanned again.

### Changes for users of pre-release builds

If you used a build from before this release:

- `--keep-fonts` is removed: extract mode always keeps extracted fonts. `--sub-assignments` is removed: use `--assignments`, which still accepts the flat `{"video.mkv": "sub.ass"}` map.
- A selection that names any video that is not a discovered MKV now fails the whole run, instead of running the valid part. Two files that resolve to the same item ID also fail the run.
- A Track Assignment applies only to the video whose path relative to `--input-dir` matches `target_video`. A bare file name no longer matches the same name in a subfolder. `target_video` and `--selected-item` values are no longer trimmed.
- A candidate with the same known season as the video now beats one with no season, for subtitles, chapters, and episode covers in the same folder.
- A bitrate with a lowercase `m` suffix (`2m`) is rejected, because FFmpeg reads it as "milli". Use `M`.
- A subtitle extracted from a file whose name has no episode number is named `<stem>_<lang>_track<id>.ass`, without a repeated stem.
- The desktop Workers and Disk I/O fields start empty and use the engine's defaults, shown as `auto (N)`, instead of fixed values.
- The inspection cache file from earlier builds still loads.
