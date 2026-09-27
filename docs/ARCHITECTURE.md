# Raam architecture

Designed 2026-09-26, from experiments 001–028 in a private lab repo
(`immich-frame-rs`), which proved every load-bearing piece on the real
hardware before this design was written. This document is the contract;
[plan/migration.md](plan/migration.md) is the route from the experiments
to this shape.

## What Raam is

Raam (Afrikaans for *frame*) turns a cheap Android photo frame into
something you own: a fast slideshow of your own photos — collages, Ken
Burns, GPU transitions, video clips, a clock — fed by
[Immich](https://immich.app), a USB stick, or photos uploaded from a
phone. It replaces the vendor app on frames like the SNUG 8″
(RK3126C, Android 6, Mali-400, 493 MB RAM), which is the first and the
minimum device.

**Roles.** Android is the product. The desktop host exists for UX
iteration (it starts instantly) and doubles as the Linux frame host. The
web build is the demo: the real UI in a browser before anyone installs.

## Fixed decisions

| Decision | Why |
|---|---|
| GLES2 / WebGL1 everywhere; no wgpu, no glow | The Mali-400 has GLES2 only; one renderer keeps the pixel match across hosts; the three hand-written GL layers already work |
| A Cargo workspace with a shared core; hosts keep only platform code | Proven by the experiments compiling shared sources; a workspace makes it real |
| The core is synchronous and single-threaded, driven by each host | wasm has no threads; determinism for simulation testing comes free |
| Every dependency must earn its keep | See [the dependency register](#the-dependency-register) |
| TigerStyle for the core | See [TIGERSTYLE.md](../TIGERSTYLE.md) |
| Root is required in v1 | Two runtime uses; mapped [below](#the-root-map) with no-root fallbacks |
| Devices: 1280×800 first, 1024×600 certified second; landscape only in v1, portrait not foreclosed | 1024×600 is the common cheap 7″ panel — the hardware Raam liberates; core takes width×height everywhere and never assumes landscape |
| Android baseline: API 23 (Android 6), armv7 | The SNUG frame; an emulator pass may later lower it |
| applicationId `io.github.noctonca.raam` | Anchored to an account already owned; F-Droid-friendly; no domain obligation |
| Licence MIT OR Apache-2.0 | Rust convention; patent grant plus simplicity |
| Releases: manual APKs now; signed releases and CI later | Reuse the proven signed-release setup when the time comes; the release key is created and backed up before the first public APK |
| v1 scope | Everything the experiments proved, plus WiFi setup, brightness, and on-device photos via USB/SD. The phone-upload page is the first post-v1 feature. "Remote" means adding photos, not remote control |
| Fresh start on the frame | Raam installs under its own id with a fresh DB; curation (hidden/favourite photos) carries over via the existing JSON export; no schema-lineage obligation to the experiments |

## The workspace

```
Cargo.toml            the `raam` package (desktop/Linux host) + workspace
crates/
  raam-model          the leaf: no dependencies
  raam-core           everything that draws and decides; wasm-clean
  raam-engine         the data engine; native only
hosts/
  android/            the product (cdylib, cfg-gated to Android)
  web/                the demo (wasm cdylib)
tools/                generators (baked palette, icon subsets)
docs/                 this design; plans in docs/plan/
```

**Dependency direction is strict:** `model ← core ← hosts` and
`model ← engine ← native hosts`. The core never sees the engine — they
meet only through traits the core defines. Nothing depends on a host.

### raam-model — the leaf

`Settings` (the one settings type), the value enums (`ScaleMode`,
`FitBackground`, `GapColour`, `VideoPlayback`, `ClockStyle`,
`TransitionChoice`), the media types (`MediaRef`, `Focus`, `MediaKind`,
`SourceKind`, `ClipInfo`), `Stats`, `DebugSwitches`, typed error enums,
and `limits.rs` — **every named limit in the product, in one file.** No
dependencies, no platform types, no I/O.

The experiments' three type-level dependency cycles all came from value
types living in GPU and UI modules; this crate is what breaks them, so
it exists from day one and stays a leaf forever.

### raam-core — draws and decides

- The egui UI: theme, kit, frame_ui, gallery, the on-screen keyboard
  (the vendored `egui_keyboard`, one copy, with its upstream LICENSE).
- The slideshow pipeline: collage layout, slide composition (Fill/Fit,
  blurred background, `RenderTarget`), Ken Burns aimed at faces,
  gl-transitions, the clock/weather overlay with its glyph atlas.
- The video orchestration over the `VideoPlayer` seam: the probe that
  brings up a clip's first frame for its tile, the live clip's start,
  pause, loop and end, one decoder at a time, the decoder-failure
  backoff and the wedged-decoder watch — plus the OES program that
  draws a decoded frame.
- **The App controller** — the product's behaviour, extracted from the
  experiment's 800-line `android_main`: input routing (tap-slop, menu
  open/close, auto-dismiss, undo-hide), the sleep/wake state machine,
  settings dirty/debounce, and the computation of the next wake
  deadline.

Rules: no threads, no I/O, no `std::time::Instant` or `SystemTime`
(they panic on wasm), no allocation after startup beyond the stated
budget. The outside world enters only through the seams.

**The GL layer** is a cfg-selected module inside the core — the same
function names and GLSL ES 1.00 shaders over three linkages:

| Target | Backend |
|---|---|
| Android | extern GLES2 |
| macOS / Linux desktop | extern desktop GL with the small shader rewrite |
| wasm32 | the WebGL1 shim (integer names → tables of WebGL objects), `gl/webgl.rs` |

This formalises what the experiments proved: the same painter renders
pixel-identically on all three (the web build matched the desktop's
`--exact` screenshots to within anti-aliased-edge rounding). The shim
implements the entry points themselves, status and info-log queries
included, so `link_program`, `RenderTarget` and the other helpers are
one copy over all three linkages. Its `web-sys` use is the core's only
platform dependency, and it is gated to wasm32.

### raam-engine — the data engine

The fetch, library, writer and weather workers; SQLite (the library
cache, settings rows, curation); HTTP (Immich REST, weather); the
providers (`Immich`, `LocalFolder`); cache cap and LRU eviction. Native
only — it is never compiled for wasm, so it may use threads, blocking
I/O and rusqlite freely. It implements the core's `TileSource` and
wakes the host through `Waker`. Its workers block on condvars/channels,
not sleep-polling.

### Hosts

| Host | Keeps |
|---|---|
| `hosts/android` (the product) | NativeActivity, EGL, input events, the decoders behind `VideoPlayer` (MediaCodec onto a SurfaceTexture, OpenSL audio and its A/V alignment, the process-wide decoder count, the reaper thread), power (wake alarm, wake lock, screen off), root helpers, storage paths |
| `raam` at the root (desktop/Linux) | winit/glutin window, the mouse as a finger, the engine's paths (the app-data dir, a photos folder), `NoVideo` and a probe that says so, env-var debug switches (`RAAM_DEBUG_VIDEO_FAIL=rt`); `--page` is the preset host with the screenshot tooling (`--exact` goldens). On a Pi it runs under X/Wayland; a bare KMS/DRM host is a possible later addition, not v1 |
| `hosts/web` (the demo) | canvas + rAF loop, its own synchronous `TileSource` over bundled sample photos (the browser decodes them; faces come from a checked-in `faces.json`), `NoVideo`, URL-query debug switches; later, "try with Immich" against demo.immich.app (CORS-open) |

## The seams

The core defines these; hosts and engine implement them. They are the
complete list of how the outside reaches the core.

| Seam | Shape | Implemented by |
|---|---|---|
| `Clock` | monotonic `now() -> Duration`, wall time, local time | host (system clocks; fake in tests) |
| `TileSource` | `take_plan / take_tile / take_failed / consumed / request / tile_is_clip` | engine (native); the web host directly |
| `MediaProbe` | clip info for a file (dimensions, codec, playability); `no_player` for a host that plays no clip at all, so the engine marks clips unplayable as it lists them instead of fetching each to find out | Android host (AMediaExtractor); the desktop's stub answers `no_player` |
| `VideoPlayer` | `open(clip, role)` → an open clip (`latch / phase / play / set_paused / set_looping / set_sound / has_frame / oes / matrix / stop`); `decoders_open` for the single-decoder rule | Android host (MediaCodec); `NoVideo` elsewhere |
| `Waker` | `wake(&self)`, `Send + Sync` | host; handed to the engine's threads |
| `Store` | settings load/save rows | engine (SQLite); localStorage or nothing on web |
| `DebugSwitches` | snapshotted **once per loop pass** | Android props / desktop env vars / web URL query |

Randomness is seeded from the injected clock plus a counter — no
`getrandom`, no wall-clock nanoseconds in core logic.

Device capability data (the RK decoder's 1920×1088 ceiling) is provided
by the host, never hard-coded in the core.

## Thread model

The core is a synchronous state machine. Each host owns its loop:

```rust
// Host loop, once per wake:
let effects = app.frame(now, &events, &mut deps);
// deps: &mut dyn TileSource, &mut dyn VideoPlayer, …
// effects: engine commands, power requests (screen off/on, wake alarm),
//          save-settings, and the next wake deadline
```

- **Android** blocks in `poll_events(deadline)`; the engine's workers
  and the video threads wake it through `Waker`.
- **Desktop** blocks in winit's event loop with the same deadline; the
  engine's workers wake it through an event-loop proxy.
- **Web** schedules via rAF and timeouts; there is no engine — the web
  `TileSource` resolves its requests from fetch callbacks.

The engine keeps its four native workers (fetch, library, writer,
weather) plus the Android host's per-clip video/audio threads. No async
anywhere: the engine's blocking internals never cross the seam, and the
core never awaits.

## Settings and persistence

One `Settings` struct in raam-model. The pipeline reads `&Settings`
directly (the experiments' per-frame copied `SlideshowSettings` dies);
the UI edits the same type (the drifted UI mirror dies).

Persistence lives in the engine: SQLite, WAL, string-keyed JSON settings
rows, schema versioned by `PRAGMA user_version` with a migrations array
— the proven shape, restarted at v1 for Raam. The frame's existing
curation arrives through the curation-JSON import; everything else is
re-entered once or re-fetched.

Secrets: the Immich API key is entered in the UI and stored in the DB
(plaintext, as the device is single-user and the DB is app-private), and
is never compiled into the binary. The experiments' baked-in `.env`
(URL, key, LAN IP pin) does not carry over.

## The root map

v1 requires root. It is used at exactly two runtime points, plus
one-time setup acts. Each entry keeps its no-root fallback on record so
a no-root mode stays a bounded, post-v1 feature — not a redesign.

| Feature | Root use | Without root (post-v1) |
|---|---|---|
| Screen off at sleep time | `su -c 'input keyevent 223'` | Drop `KEEP_SCREEN_ON` and accept the system timeout, or a device-admin `lockNow` (needs a dex component) |
| Storage permission for the photos folder | one-time `su -c 'pm grant …'` | `adb shell pm grant` at setup, the runtime permission dialog (risks a background kill on low-RAM builds), or the app-specific external dir |
| Disable the vendor app | one-time `pm disable-user` | Leave it enabled; Raam is still the user-chosen launcher |
| Become the home app | none — it's a manifest category + user choice | same |

The low-memory killer on these frames kills any non-listed app that
loses the foreground; being the home app covers it, because the system
restarts the home app within about a second — which is also what the
panic policy relies on (crash → exit(70) → relaunched).

## Video

Android-only in v1, behind `MediaProbe` + `VideoPlayer`. The core runs
the orchestration the lab proved — probe-then-play, one decoder at a
time (the live open waits for the probe's release), exponential backoff
on failure, the wedged-VPU watch — as a state machine with simulation
tests over a fake player. The Android host keeps the decoders
themselves: MediaCodec onto a SurfaceTexture, the OpenSL audio and its
A/V alignment against the audio clock, and the reaper thread that
releases a stopped decoder off the render thread. Desktop and web use
the core's `NoVideo`, whose `open` always fails, and neither ever plans
a clip: the web source offers none, and the desktop's probe answers
`no_player`, so its engine marks every clip unplayable as it lists it
rather than downloading each to find out. If a Linux frame ever
needs clips, that's a new `VideoPlayer` implementation (GStreamer or
V4L2), not a core change.

Clips the device can't decode are probed, marked unplayable in the DB,
and skipped — server-side transcode policy is the user's lever, not
Raam's problem.

## Memory budget

The design target is the measured envelope of the final experiment on
the real frame, as named limits in `raam-model::limits`:

| Budget | Value | Source |
|---|---|---|
| App total (PSS + GPU + window buffers) | ≤ 80 MB steady | measured ~67 MB |
| Mali peak | ≤ 64 MB | measured 63.5 MB peak |
| Tile textures per plan | ≤ 12 MB, reserved up front | measured max 12.2 MB |
| Transition scratch | 2 × screen-sized RT, allocated once | ~4 MB each at 1280×800 |

"Allocate up front" is aimed squarely at the one observed failure mode:
GPU allocation failing when a plan composes during a transition under
memory pressure. Reserving the tile and scratch targets once removes
the per-plan allocation that raced. Every GPU allocation failure path
stays survivable regardless (drop the plan, retry, cut the transition)
— that recovery is proven and must survive the port.

## Photos without Immich

- **v1: USB stick / SD card**, through the `LocalFolder` provider
  (hash, EXIF orientation and date, face-free centre-weighted preview).
- **Post-v1, first in line: the upload page.** A small HTTP server on
  the frame; the frame shows a QR code with its LAN URL; the page
  accepts photo uploads into the local folder. Pairing and upload are
  one feature. LAN only — "remote" means adding photos, and beyond the
  LAN that becomes a relay question for much later.
- SMB/WebDAV: maybe, later.

## The web demo

The real core compiled to wasm — not a mock: the App controller, the
pipeline and the chrome, as the frame runs them. Bundled CC0 sample
photos by default, stored as previews the way Immich serves them
(uncropped JPEGs, short side 1080 at q75: 4.5 MB for twenty, and only
a portrait shown alone is upscaled, by 1.2×), with their faces in a checked-in `faces.json` that a detector
wrote once (`tools/web-faces.swift`, Apple's Vision), so Ken Burns and
the Fill crop aim as they do on the frame. The web `TileSource` offers no clips.
Settings live for the page's lifetime only; the `Store` seam's
localStorage option waits until it's wanted.

Later, "try with Immich" runs against demo.immich.app straight from the
browser (its CORS allows it). A user's own server generally won't allow
a browser origin, so the demo will make that limit clear. The wasm is
built `opt-level = "s"` through a root `[profile.web]` (profiles live at
the workspace root only) and runs through wasm-opt when it's installed.

## Testing

Four layers, cheapest first:

1. **Unit** — model, core and engine are rlibs; `cargo test --workspace`
   runs on the dev machine: collage layouts, schedule math, parsers,
   settings round-trips, controller transitions.
2. **Deterministic simulation** — the TigerStyle payoff: a fake `Clock`,
   a scripted `TileSource`, injected allocation and decoder failures,
   driving the App through days of virtual time in milliseconds,
   asserting the invariants (never two decoders; sleep/wake honoured;
   backoff caps; budgets never exceeded; recovery after every injected
   failure). Logic-only, no GL.
3. **Pixel** — the desktop host's `--exact` golden screenshots, and the
   headless-Chrome diff of the web build against them. Both proven in
   the experiments; they are the render-regression net and the pixel
   match's enforcement.
4. **On-frame** — manual, per migration step: the checked items in
   [plan/migration.md](plan/migration.md), always including the
   GPU-failure injection run.

**CI** (GitHub Actions): fmt, clippy (`-D warnings`), `cargo test
--workspace`, and a wasm build, on every push and PR. PR titles follow
Conventional Commits, CI-enforced. APK builds stay manual until
releases are automated.

## The dependency register

Every dependency earns a row here before it enters a `Cargo.toml`; a
dependency without a row is a review failure. The audit of the
experiments (private lab repo, `docs/raam-prep/dependencies.md`) is the
basis; its replacements are adopted as decisions:

| Crate | Where | Why it earns its keep |
|---|---|---|
| `egui` =0.36.x, **no default features** | core | The UI. Default fonts off everywhere (−1.4 MB; the theme ships Roboto + Material Symbols subsets) |
| `egui_keyboard` (vendored, one copy) | core | On-screen keyboard tuned for finger-on-frame; upstream LICENSE included |
| `log` | all | The logging facade |
| `rusqlite` (bundled, `LIBSQLITE3_FLAGS` trimmed) | engine | The library DB; FTS/RTREE/etc. compiled out (−0.5 MB) |
| `ureq` 3 (rustls) | engine | Blocking HTTP without the async runtime (replaces reqwest: −60 crates); TLS stays because weather and users' Immich need it, and API 23's trust store is stale so roots are bundled |
| `serde_json` (Value only, no derive) | engine | Immich/weather parsing, settings rows |
| `jpeg-decoder` + `jpeg-encoder` | engine | One decoder (DCT-scaled reads) and a dependency-free encoder for previews (replaces `image`) |
| `kamadak-exif` | engine | Orientation + DateTimeOriginal; EXIF edge cases are cheap insurance |
| SHA-1 via `ring::digest` | engine | Immich checksum matching; ring is already there under rustls |
| `fontdue` | core (overlay) | Clock/weather atlas rasterisation; revisit merging onto egui's skrifa |
| `android-activity`, `ndk`, `ndk-sys`, `jni` 0.21, `android_logger` (no defaults), `libc` | android | The platform glue; no regex logger filter. jni stays on 0.21 until its 0.22 API redesign is ported deliberately (it duplicates android-activity's 0.22, ~56 KB) |
| `libc` | engine; raam (desktop) | `statvfs` (free space for the cache cap) and `mktime` (EXIF local times); the engine is native-only by design. The desktop's `localtime_r`, the Clock seam's local time, as the Android host does it |
| OpenSL bindings, checked in | android | Pre-generated and pruned; no bindgen, no libclang at build time |
| `winit`, `glutin`, `glutin-winit`, `egui-winit` (no `links`), `png` | raam (desktop) | The window host and the screenshot tool |
| `wasm-bindgen`, `js-sys`, `web-sys` | web; core on wasm32 only (the WebGL1 module) | Unavoidable wasm glue, and egui already brings all three on wasm32. The web host logs and reports panics to the console itself (about 20 lines), so no `console_log` or `console_error_panic_hook` |
| Baked colour table (generator in `tools/`) | core | Replaces `material-colors` at runtime (−20 crates, −getrandom); a test asserts the table matches the derivation |

Notably absent, by decision: wgpu, glow, tokio, reqwest, `image`,
bindgen-at-build-time, material-colors-at-runtime, clap (hosts parse
their few flags by hand), any async runtime.

Licences: all permissive; ship the notices (Apache-2.0 fonts, ring,
webpki-roots' CDLA-Permissive-2.0).

## What this design deliberately defers

No-root mode; the upload page (designed for, not built); portrait;
KMS/DRM host; video off Android; SMB/WebDAV; relay-based remote;
automated releases and F-Droid; a lower Android baseline than API 23.

Each is deferred, not foreclosed: the seams above are where each one
plugs in.
