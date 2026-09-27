# Migration: from the experiments to Raam

The route from the lab repo's experiment 027 (the proven Android app)
plus 026 (the design system and frame_ui) and 028 (the web preview)
into this workspace. It amends the lab's split-plan draft with the
decisions of the 2026-09-26 architecture session: the workspace is
real, the App controller extraction is its own step, and the target
shapes are those of [../ARCHITECTURE.md](../ARCHITECTURE.md).

The lab repo (`immich-frame-rs`, private) stays the working tree for
steps 1–4, which refactor 027 in place where the on-frame checks are
cheapest. Code starts landing in this repo at step 5. Every risky step
is verified on the real frame before the next, including the
GPU-failure injection run (`debug.video.fail=rt`) — the recovery paths
are the part bought with the most debugging.

| # | Step | Size | Verified on the frame |
|---|---|---|---|
| 1 | Portable time and randomness inside 027: every `Instant`/`SystemTime` behind the `Clock` seam; fault injection becomes a host-set flag | S | dwell, pause, Ken Burns, transition length, `fail=rt` recovery |
| 2 | Video out of the slideshow into `video.rs` (~600 lines): OES program, probe/live players, backoff, wedge watch | L — riskiest | A/V start, Next mid-clip per playback mode, **one decoder at a time**, memory vs baseline |
| 3 | `TileSource` trait and a generic `Plan`; fetch behind the seam | M | Prev, Hide/undo during a plan build, collage max, first plan at boot |
| 4 | Overlay split: local time via `Clock`, weather icons renamed | S | three clock styles, 12/24 h, weather |
| 5 | **The workspace move**: `raam-model` (settings type, value enums, limits — breaks the type cycles), then the portable modules into `raam-core`; the fetch/library/db/immich side into `raam-engine`; 027 becomes a thin `hosts/android` | L | smoke run, settings survive restart |
| 5b | **Extract the App controller** from `android_main` (~800 lines): input routing, overlay lifecycle, sleep state machine, wait computation — into `raam-core`, events in / effects out | M–L | tap routing, auto-dismiss, undo-hide, sleep/wake cycle, manual wake |
| 6 | frame_ui replaces the 025-era `ui.rs`: one Settings type ends the mirror drift; adds the video settings frame_ui lacks | M–L | menu latency, typing, settings survive restart, MemFree with menu open during a clip |
| 6b | `VideoSeam` narrows to ARCHITECTURE's `VideoPlayer` (open/play/pause/stop/phase/has_frame/oes/matrix): the probe/live/backoff/wedge orchestration moves into the core (split out of step 6, 2026-09-27) | M | A/V start, Next mid-clip per playback mode, one decoder at a time, `fail=rt` recovery |
| 7 | The web host adopts the core: bundled photos + faces.json through its own `TileSource` | M | collages, Fill/Fit, blur, clock in the browser; pixel diff vs desktop goldens |
| 8 | The desktop host adopts the core (the `raam` binary); goldens re-blessed once, knowingly | S–M | `--exact` suite green |

Step-5 details settled in advance by the workspace check in the lab
repo: cargo-apk2 builds workspace members into the root `target/`;
Android deps are cfg-gated (`[target.'cfg(target_os = "android")']` +
`#![cfg]`) so `cargo check --workspace` stays green on the dev machine;
profiles live at the root only, so the web build gets `[profile.web]`.

Dependency swaps (reqwest→ureq, drop `image`, baked palette, checked-in
OpenSL bindings, egui default features off, SQLite flag trim) happen as
the owning module crosses into this repo — each swap is its own commit
with its register row.

Fresh start on the frame: Raam installs as `io.github.noctonca.raam`
with a fresh DB (schema v1); the curation carries over via the lab's
`frame-curation.json` export/import. The experiments repo is then the
archive it was always going to be.

**Risks carried over from the lab draft:** the single-decoder rule and
the `glFinish`/FBO ordering in step 2; every GPU-failure path surviving
steps 2, 3 and 5; frame_ui's font memory in step 6 (measure on-frame
before and after).

Estimated 3–5 sessions.
