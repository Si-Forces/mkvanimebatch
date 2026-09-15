# 0010. Mainline Rust Engine Cutover and Python Pipeline Decommissioning

## Context
Following the implementation of the pure Rust media engine (ADR-0005, ADR-0006), the in-process Engine Bridge (ADR-0007), typed track assignments (ADR-0008), and streaming progress telemetry (ADR-0009), the legacy Python prototype codebase (`mkv_*.py`), PyInstaller sidecar compilation scripts, and pytest suites remained in the repository root to verify behavioral parity during development. With all 142 Rust engine/CLI tests, 32 in-process bridge tests, and 80 UI contract tests passing with 100% contract fidelity, retaining the Python codebase creates unnecessary cognitive overhead, duplicate CI overhead, and maintenance friction.

## Decision
We decided to decommission the Python implementation entirely and cut over the repository to a pure Rust/Tauri architecture for the `v1.0.0` release:
1. **Legacy Deletion**: Completely remove all root `mkv_*.py` files, `tests/` pytest suites, synthetic benchmarks, Windows batch wrappers, and Python-specific configuration files (`pyproject.toml`, `pyrightconfig.json`). Historical implementation details remain preserved in Git history.
2. **Workspace Package Centralization**: Centralize package metadata in the root `Cargo.toml` under `[workspace.package]`, establishing unified `1.0.0` semantic versioning across `mkv-core`, `mkv-cli`, and `desktop-tauri`.
3. **Unified CI Gate**: Replace `.github/workflows/ci.yml` with a consolidated matrix workflow (Ubuntu 24.04 and Windows) executing `cargo fmt --all`, workspace-wide `clippy` and `test`, standalone `mkv-cli` smoke tests, and frontend UI tests, guarded by a single required `ci-status` gate. Retire `.github/workflows/rust-ci.yml`.
4. **Canonical Release Workflow**: Overhaul `.github/workflows/release.yml` for `v*` tags to build and publish self-contained Tauri bundles (AppImage, deb, rpm, portable Windows zip/exe) alongside standalone `mkv-cli` binaries with SHA256 checksums, retiring `.github/workflows/rust-unstable-release.yml`.
5. **Repository Governance**: Update `AGENTS.md` and `README.md` to establish workspace `cargo` and Node UI test gates as the canonical development protocol.

## Consequences
- The repository is 100% self-contained Rust, HTML/CSS/JS, and GitHub Actions; Python is no longer required in development environments or CI runners.
- Build times in CI and release workflows are dramatically reduced by eliminating Python matrix runs and PyInstaller sidecar builds.
- Contributors interact strictly with `mkv-cli` for CLI operations and `desktop-tauri` for desktop GUI operations.
