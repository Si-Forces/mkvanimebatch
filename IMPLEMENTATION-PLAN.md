# Implementation Plan: Selected Items Execution Queue, Concurrency Pipelining & Enriched Track Assignment

This document provides complete, step-by-step instructions for worker agents to implement the architectural decisions established in [ADR-0011](file:///home/sifaha/My%20Project/mkvanimebatch/docs/adr/0011-selected-items-queue-and-concurrency-pipelining.md), adhering to the domain glossaries in [crates/mkv-core/CONTEXT.md](file:///home/sifaha/My%20Project/mkvanimebatch/crates/mkv-core/CONTEXT.md) and [desktop-tauri/CONTEXT.md](file:///home/sifaha/My%20Project/mkvanimebatch/desktop-tauri/CONTEXT.md).

---

## 1. Overview & Architectural Requirements

### Key Deliverables
1. **`BatchRequest` Contract Evolution**: Add `selected_items: Option<Vec<String>>` for explicit execution subsets and custom ordering with strict preflight fail-fast validation.
2. **Dual-Governor Global Work Pipelining**:
   - Bounded concurrent preflight video probing using a probe semaphore (`(workers * 2).clamp(2, 16)`).
   - Unified global `JoinSet` bounded by `workers` across all release directories (eliminates directory-by-directory stall).
   - `io_semaphore` guarding remux disk writing (`mkvmerge`), while CPU-bound audio transcoding (`ffmpeg`) executes within the worker slot.
3. **Queue Selection & Card Reordering**: Checkboxes on episode cards, `▲` / `▼` card reordering, `Select All` / `Deselect All` bulk actions, dynamic `X / Y Selected` badge telemetry, and disabling Start on zero selection.
4. **Enriched Track Assignment GUI**: Modal upgraded with native file dialog browsing (`rfd`), ISO-639-2 language tag, track title, and millisecond time offset controls.
5. **Zero Regressions & Full Gate Pass**:
   - `cargo fmt --all -- --check`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - `cargo test --workspace`
   - `node --check desktop-tauri/ui/app.js`
   - `node --test desktop-tauri/ui/*.test.js`

---

## 2. Step-by-Step Implementation Instructions

### Step 1: Media Engine (`crates/mkv-core`)

#### File: `crates/mkv-core/src/request.rs`
1. **Struct Update**:
   Add `selected_items` to `BatchRequest`:
   ```rust
   /// Optional ordered subset of relative video paths or item IDs designating active execution.
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub selected_items: Option<Vec<String>>,
   ```
   Ensure `Default for BatchRequest` initializes `selected_items: None`.

2. **Validation Logic (`BatchRequest::validate`)**:
   In `pub fn validate(&self) -> Result<(), String>`:
   - When `self.selected_items` is `Some(ref items)`:
     - If `items.is_empty()`, return `Err("selected_items cannot be empty when specified".into())`.
     - Check for duplicates and path traversal:
       ```rust
       let mut seen_items: std::collections::HashSet<String> = std::collections::HashSet::new();
       for item in items {
           let norm = item.trim().replace('\\', "/");
           let path = std::path::Path::new(&norm);
           if norm.is_empty()
               || norm.starts_with('/')
               || path.has_root()
               || is_drive_prefix(&norm)
               || !path.is_relative()
               || path.components().any(|c| matches!(c, std::path::Component::ParentDir))
           {
               return Err(format!("Invalid relative path in selected_items: '{item}'"));
           }
           let clean = assignment_clean_path(&norm); // or normalize component parts
           if !seen_items.insert(clean.clone()) {
               return Err(format!("Duplicate entry in selected_items: '{clean}'"));
           }
           let full_path = self.input_dir.join(&clean);
           if !full_path.is_file() {
               return Err(format!("Selected item does not exist: '{}'", full_path.display()));
           }
       }
       ```
     - Cross-validate with `self.assignments`:
       If `let Some(ref assignments) = self.assignments`:
       For each `assignment` in `assignments`:
       ```rust
       let clean_target = assignment.clean_target_video();
       if !seen_items.contains(&clean_target) {
           return Err(format!(
               "Track assignment targets video '{clean_target}' which is not in selected_items"
           ));
       }
       ```

3. **Pipeline Config Translation**:
   In `BatchRequest::build_pipeline_config`:
   Set `config.selected_items = self.selected_items.clone();`.

4. **Unit Tests**:
   Add tests in `crates/mkv-core/src/request.rs`:
   - `test_validate_selected_items_success()`
   - `test_validate_selected_items_empty_fails()`
   - `test_validate_selected_items_duplicate_fails()`
   - `test_validate_selected_items_traversal_fails()`
   - `test_validate_selected_items_nonexistent_fails()`
   - `test_validate_assignment_not_in_selected_items_fails()`

---

#### File: `crates/mkv-core/src/pipeline.rs`
1. **Config Struct**:
   Add `pub selected_items: Option<Vec<String>>` to `PipelineConfig` and initialize as `None` in `Default`.

2. **Filtering & Ordering in `run_remux` & `run_extract`**:
   After `let files = scan_mkv_files(&self.config.input_dir, true, None);`:
   ```rust
   let files: Vec<ParsedFile> = if let Some(selected) = &self.config.selected_items {
       let mut file_map: HashMap<String, ParsedFile> = HashMap::new();
       for f in files {
           let id = get_item_id(&f.path, &self.config.input_dir);
           let rel_path = f.path.strip_prefix(&self.config.input_dir)
               .map(|p| p.to_string_lossy().replace('\\', "/"))
               .unwrap_or_else(|_| f.name.clone());
           file_map.insert(id.clone(), f.clone());
           file_map.insert(rel_path, f);
       }
       let mut ordered = Vec::with_capacity(selected.len());
       for item in selected {
           let norm = item.trim().replace('\\', "/");
           if let Some(f) = file_map.remove(&norm) {
               ordered.push(f);
           }
       }
       ordered
   } else {
       files
   };
   ```
   Ensure `summary.total = files.len();` and `JobEvent::BatchStart` reflects this filtered list.

3. **Concurrent Probing**:
   In `run_remux`:
   Replace sequential directory probe `for v in videos { get_or_probe_video(...) }` with bounded asynchronous probing using `JoinSet` and a semaphore:
   ```rust
   let probe_limit = (max_workers * 2).clamp(2, 16);
   let probe_sem = Arc::new(tokio::sync::Semaphore::new(probe_limit));
   let mut probe_set = tokio::task::JoinSet::new();
   for v in videos {
       let path = v.path.clone();
       let cache = cache_opt.cloned(); // Arc or thread-safe reference
       let sem = Arc::clone(&probe_sem);
       probe_set.spawn(async move {
           let _permit = sem.acquire().await.ok();
           let res = get_or_probe_video(&path, cache.as_deref());
           (path, res)
       });
   }
   let mut dir_inspections: HashMap<PathBuf, InspectionResult> = HashMap::new();
   while let Some(res) = probe_set.join_next().await {
       if let Ok((path, probe_res)) = res {
           match probe_res {
               Ok(meta) => { dir_inspections.insert(path, InspectionResult::success(meta)); }
               Err(e) => { dir_inspections.insert(path, InspectionResult::failure(e)); }
           }
       }
   }
   ```

4. **Global Work Pipelining**:
   Instead of executing tasks directory-by-directory in `dir_list`:
   - Phase 1: Iterate over `dir_list`, run `prepare_release_directory` and asset resolution, accumulating all `(PlannedJob, TrackSelection)` into a single `all_planned_jobs: Vec<(PlannedJob, TrackSelection)>`. (Handle dry-run and skips as they occur).
   - Phase 2: Execute all `all_planned_jobs` inside a single unified `JoinSet` loop bounded by `max_workers` using `io_semaphore` permits guarding `execute_remux_job`.

---

### Step 2: Desktop Tauri Backend (`desktop-tauri/src-tauri`)

#### File: `desktop-tauri/src-tauri/src/commands.rs`
1. **New File Picker Command**:
   ```rust
   #[tauri::command]
   pub fn pick_subtitle_file() -> Option<String> {
       rfd::FileDialog::new()
           .add_filter("Subtitle Files", &["ass", "ssa", "srt", "vtt", "sub"])
           .pick_file()
           .map(|p| p.to_string_lossy().into_owned())
   }
   ```

#### File: `desktop-tauri/src-tauri/src/lib.rs`
1. Register `commands::pick_subtitle_file` in `invoke_handler`:
   ```rust
   .invoke_handler(tauri::generate_handler![
       commands::inspect_directory,
       commands::run_job,
       commands::stop_job,
       commands::clear_cache,
       commands::pick_directory,
       commands::pick_folder,
       commands::pick_subtitle_file,
       commands::get_preset_profiles,
       commands::get_job_snapshot,
       // ... other window/telemetry commands
   ])
   ```

---

### Step 3: Desktop UI State & Presentation (`desktop-tauri/ui`)

#### File: `desktop-tauri/ui/queue-state.js`
1. **Item Selection Field**:
   In `createQueueState`: initialize `selected: true` on each item in `state.items`.
2. **State Helper Functions**:
   Export functions:
   - `toggleItemSelection(state, id, selected)`: Updates `item.selected` (defaulting to `!item.selected`).
   - `selectAll(state)`: Sets `selected = true` for all items.
   - `deselectAll(state)`: Sets `selected = false` for all items.
   - `reorderItem(state, id, direction)`: Moves `id` in `state.order` earlier (`'up'`) or later (`'down'`).
   - `getSelectedCount(state)`: Returns count of items where `selected === true`.
3. **Tests (`desktop-tauri/ui/queue-state.test.js`)**:
   Add test coverage for `toggleItemSelection`, `selectAll`, `deselectAll`, and `reorderItem`.

---

#### File: `desktop-tauri/ui/queue-view.js`
1. **Card Controls in `createCardElement`**:
   Add checkbox and reordering buttons into episode cards:
   ```javascript
   const checkHtml = `<input type="checkbox" class="ep-select-check" checked data-item-id="${id}" title="Include in batch" />`;
   const reorderHtml = `
     <div class="ep-reorder-group">
       <button type="button" class="ep-reorder-btn ep-up-btn" data-item-id="${id}" title="Move up">▲</button>
       <button type="button" class="ep-reorder-btn ep-down-btn" data-item-id="${id}" title="Move down">▼</button>
     </div>
   `;
   ```
2. **DOM Event Handling**:
   - Checkbox change listener: updates `state` item selection and triggers queue count render.
   - Up/down button click listeners: reorders DOM cards, updates `state.order`, and triggers queue count render.
3. **Queue Header & Count Badge**:
   Render `${selectedCount} / ${totalCount} Selected` when manifest is active.
4. **Selection Accessor**:
   ```javascript
   getSelectedOrderedItemIds() {
     if (!this.state) return [];
     return this.state.order.filter(id => {
       const item = this.state.items.get(id);
       return item && item.selected !== false;
     });
   }
   ```
5. **Tests (`desktop-tauri/ui/queue-view.test.js`)**:
   Add test coverage for selection toggle and reordering.

---

#### File: `desktop-tauri/ui/index.html` & `desktop-tauri/ui/style.css`
1. **Bulk Queue Controls**:
   In `.col-queue .panel-header`, add bulk action buttons:
   ```html
   <div class="queue-bulk-actions" id="remux-queue-bulk">
     <button type="button" class="btn btn-xs" id="remux-select-all">Select All</button>
     <button type="button" class="btn btn-xs" id="remux-deselect-all">Deselect All</button>
   </div>
   ```
2. **Enriched Subtitle Assignment Modal (`#sub-assign-modal`)**:
   Expand modal structure:
   - Candidate list container (`#sub-candidate-list`).
   - File picker row: `<input type="text" id="sub-custom-path" readonly placeholder="Choose custom file..." /><button type="button" id="sub-browse-btn" class="btn btn-sm">📁 Browse...</button>`.
   - Track parameters:
     - Language: `<select id="sub-lang-select"><option value="ind">Indonesian (ind)</option><option value="eng">English (eng)</option><option value="jpn">Japanese (jpn)</option><option value="custom">Custom...</option></select>` and text `<input id="sub-lang-custom" placeholder="ISO-639-2 tag" />`.
     - Title: `<input id="sub-title-input" placeholder="Track Title (e.g. Dialogue)" />`.
     - Offset: `<input id="sub-offset-input" type="number" step="100" placeholder="0" /> ms`.
   - Action buttons: `Save Assignment`, `Clear Assignment`, `Cancel`.
3. **Card Styling (`style.css`)**:
   Add styling for `.ep-select-check`, `.ep-reorder-group`, `.ep-reorder-btn`, and modal field rows.

---

#### File: `desktop-tauri/ui/app.js` & `desktop-tauri/ui/job-controller.js`
1. **Track Assignment Model in `app.js`**:
   Update modal logic to store structured objects in `tracker.assignments`:
   ```javascript
   {
     target_video: targetVideo,
     asset_path: selectedPath,
     track_type: "subtitle",
     language: selectedLang || null,
     title: trackTitle || null,
     offset_ms: offsetMs || null
   }
   ```
2. **Native File Browser Hook**:
   Attach click listener to `#sub-browse-btn` calling `invoke('pick_subtitle_file')`.
3. **Job Controller Execution (`job-controller.js`)**:
   In `start()` / `buildBatchRequest()`:
   ```javascript
   const selectedIds = tracker.getSelectedOrderedItemIds();
   if (selectedIds.length === 0) {
     throw new Error("No episodes selected in queue");
   }
   batchRequest.selected_items = selectedIds;
   ```
   Filter `batchRequest.assignments` to only include assignments for items in `selectedIds`.
4. **Button State**:
   Update start button enablement: disable when `selectedCount === 0`.
5. **Tests (`desktop-tauri/ui/job-controller.test.js`)**:
   Update test suite verifying `selected_items` serialization and assignment filtering.

---

## 3. Verification & Quality Gates

Run the following gates in order:

```bash
# 1. Rust formatting & linting
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# 2. Rust test suite
cargo test --workspace

# 3. Frontend JS syntax & tests
node --check desktop-tauri/ui/app.js
node --test desktop-tauri/ui/*.test.js
```

Ensure all tests pass with zero warnings before declaring implementation complete.
