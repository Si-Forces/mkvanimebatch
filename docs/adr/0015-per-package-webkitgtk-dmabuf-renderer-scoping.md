# 0015. Per-Package WebKitGTK DMABUF Renderer Scoping

> **Status**: Accepted
> **Date**: 2026-09-27
> **Context**: Desktop Application Context

## Context

The Desktop Application's Linux window is declared `"transparent": true` in `desktop-tauri/src-tauri/tauri.linux.conf.json`, and `init_linux_env()` in `desktop-tauri/src-tauri/src/lib.rs` forces two environment variables before the Tauri builder runs:

- `WEBKIT_DISABLE_DMABUF_RENDERER=1`, a workaround for an upstream WebKitGTK/Mesa EGL race that segfaults in `SkiaGLContext` / `__eglFini` during shutdown.
- `GDK_BACKEND=x11,wayland`, a workaround for a `WebKitWebProcess` `EGL_BAD_PARAMETER` abort under native Wayland.

Both workarounds were originally applied unconditionally to every Linux run. That turned out to be wrong for the DMABUF one, and the failure mode was subtle enough to be misdiagnosed as a frontend bug.

On a **Native Package** install running **WebKitGTK 2.54+**, disabling the DMABUF renderer drops Skia to CPU shared-memory rendering. Because the window is transparent, that path only recomposites pointer-damaged regions: the tile under the mouse cursor redraws while the remainder of the window stops updating entirely, showing the desktop through the unpainted surface. The window's geometry never changes — only whether its pixels are composited — so the symptom reads as an intermittent flicker or as "only the item under my mouse is displayed".

The AppImage build was unaffected, which made this look like a packaging bug rather than a code bug. It is neither: `release.yml` builds `appimage`, `deb`, and `rpm` from a single `cargo tauri build` on `ubuntu-24.04`, so all three ship identical code and identical window configuration. Only the *runtime* differs:

| | AppImage | Native RPM (Fedora 44) |
| --- | --- | --- |
| GTK 3 | 3.24.41 (bundled) | 3.24.52 (system) |
| WebKitGTK | bundled; version floats with the CI runner | 2.54.0 (system) |
| ICU | 74 (bundled) | 77 (system) |
| GDK backend | XWayland | XWayland |

Throughout this record, **AppImage** means the self-contained bundle that ships its own GTK/WebKitGTK, and **Native Package** means a system package (RPM, DEB) that links against the host distribution's libraries. The two are the discriminator for every decision below.

These were read off the two installed builds on the reporting host, not from a fixture, and the AppImage column is only as good as the build that produced it. The AppImage's WebKitGTK version is not pinned by `release.yml`, which installs `libwebkit2gtk-4.1-dev` from the `ubuntu-24.04` runner image without a version constraint. The exact number is therefore not a stable property of the build and this ADR does not assert one.

Both builds run through the same XWayland path — `xprop -root _NET_CLIENT_LIST` lists the app window as an X11 client (`WM_CLASS = mkv-anime-batch`) in both cases — so `GDK_BACKEND` is not the differentiator.

What *is* established, by direct experiment rather than inference: forcing `WEBKIT_DISABLE_DMABUF_RENDERER=0` on the affected native build makes the window behave correctly, while leaving it `1` reproduces the dropped regions. The DMABUF renderer is therefore causal. Whether the AppImage is unaffected because its bundled WebKitGTK predates the behaviour change, or for some other reason in its older GTK/WebKit stack, was not isolated further — it is the most likely explanation given the version split, but it is not proven, and the decision below does not depend on it being right.

## Decision

We decided to scope the DMABUF workaround to the packaging boundary that actually needs it, and to treat the AppImage/runtime split as the discriminator rather than a distribution check:

1. **`WEBKIT_DISABLE_DMABUF_RENDERER=1` is set only inside AppImage environments.** The guard is the presence of `APPIMAGE` or `APPDIR` in the environment, both of which the AppImage runtime sets. A Native Package leaves DMABUF enabled, restoring hardware-accelerated damage tracking on transparent windows.

2. **The DMABUF workaround remains a default, never an override.** `init_linux_env()` guards it with an `is_none()` check, so an operator-supplied value always wins. This is deliberate: the escape hatch is how a user disables a workaround that breaks on their specific host. `GDK_BACKEND` is guarded the same way and enjoys the same property. The one variable that does *not* is `LD_PRELOAD`, which is a `contains()` check rather than an `is_none()` check (see decision 4), so the "operator value always wins" property does not extend to it.

3. **`GDK_BACKEND=x11,wayland` is applied by default to all Linux runs, not only AppImages.** It is unrelated to this failure, and the fix was validated with the backend in place. Note this is a *default*, not an enforcement: like the DMABUF workaround it is wrapped in an `is_none()` check, so an operator who exports `GDK_BACKEND` keeps their own value. It is described here because it is one of the two forced-looking variables a reader of `init_linux_env()` will be checking, not because this decision changed it.

4. **The `libwayland-client` `LD_PRELOAD` injection remains AppImage-only**, for the same packaging reason: the AppImage bundles an older `libwayland-client.so.0` that conflicts with the host's Mesa EGL drivers. This one is not an `is_none()` default — it is guarded by `!current_preload.contains("libwayland-client")`, so an operator who has already preloaded a `libwayland-client` is left alone, but one who has preloaded something else gets their value prepended to rather than replaced.

5. **The scoping is locked by a single serialised test.** `init_linux_env()` reads and writes process-global environment, so the test that covers it lives in one test function. Split across two, the tests clobber each other's `APPIMAGE` state: measured at 14 failures in 20 runs when the suite is filtered to just those two tests, and 0 in 20 when all 41 lib tests run together, which is why the defect stayed invisible on the full-suite CI path. Merging the two cases into one took the lib test count from 42 to 41, which is why the filtered measurement and the full-suite count differ by one. It asserts the native default and both AppImage markers unconditionally; the `libwayland-client` preload assertion is conditional on this host actually having one of the candidate paths, so on a host without it that assertion does not run.

## Considered Options

- **Drop the DMABUF workaround entirely**: *Rejected*: reintroduces the `SkiaGLContext` / `__eglFini` shutdown segfault in AppImage, which is the environment that motivated the workaround in the first place. Scoping keeps both behaviours intact and leaves the AppImage path byte-for-byte unchanged.
- **Set `"transparent": false` on Linux**: *Rejected for now, retained as a fallback*: it would remove the entire class of transparent-surface bugs rather than managing it, at the cost of the 12px rounded window corners the design depends on. Worth revisiting if WebKitGTK's transparent path regresses again.
- **Detect the distribution instead of the packaging format** (e.g. `/etc/os-release`, or a WebKitGTK version probe): *Rejected*: distro sniffing is brittle across Fedora/Ubuntu/Arch derivatives, and a version probe is guesswork. The AppImage-versus-native split is the actual boundary — it is the thing that determines which WebKitGTK the process will load — and it is already exposed as an environment variable.
- **Leave the workaround unconditional and wait for an upstream fix**: *Rejected*: the defect is user-visible today on the primary distribution, and upstream will not treat an application-level configuration mistake as its bug.
- **Detect the regression at runtime and toggle the renderer**: *Rejected*: there is no reliable signal from inside the webview that distinguishes "not damaged yet" from "compositor dropped the surface", so the app would have to guess.

## Consequences

- Native Package installs keep hardware-accelerated damage tracking, and the transparent window stops dropping undamaged regions.
- AppImage behaviour is unchanged: it still receives `WEBKIT_DISABLE_DMABUF_RENDERER=1` and the `libwayland-client` preload, so its shutdown safety is unaffected by this decision.
- **Operators who exported `WEBKIT_DISABLE_DMABUF_RENDERER=1` themselves keep the rendering bug**, because decision 2 preserves their override. The remedy is to unset the variable, not to widen the default. This is the one support case this decision knowingly leaves open.
- The WebKitGTK version is now load-bearing in both directions. A future upstream change to the transparent-window rendering path must be re-validated on *both* package types: the AppImage on whatever its runner bundled, and a native install on the distribution's system WebKitGTK. A regression that only reproduces on one of them will be masked by the other.
- **The AppImage runtime is not pinned, so it is a latent regression source.** When the `ubuntu-24.04` runner image eventually ships a WebKitGTK with this same transparent-window defect, the AppImage will start exhibiting the identical bug — while still carrying the DMABUF workaround that is the very thing causing it. The AppImage-only guard is correct today but does not protect against that. If the AppImage starts flickering, re-check whether the bundled WebKitGTK has crossed the 2.54 behaviour before touching this decision.
- **The AppImage runtime is now recorded, not just described.** `release.yml` writes `APPIMAGE-RUNTIME.txt` into the release, listing the exact WebKitGTK, JavaScriptCoreGTK and GTK versions bundled, and the release body embeds the same table. This is diagnosis, not prevention: it makes a release that inherited the defect above identifiable from its own manifest instead of mysterious. The release also **fails** if any of those three libraries is missing from the AppDir, because a missing bundle means silent fallback to the host runtime — which is the same latent regression arriving by a different route. Neither measure pins the version; that remains open.
- The AppImage shutdown workaround should be re-checked periodically against current WebKitGTK. Once the upstream `SkiaGLContext` race is fixed, the AppImage-only guard and the preload injection can both be retired together.
- The env-scoping test must remain a single test function. Anyone extending it should extend that one test rather than splitting the cases, or reintroduce a flake that is invisible on the full-suite CI path and appears only when the tests are run filtered.
