# Next

What's planned, roughly in order. A larger item gets its own plan in
this directory when work on it starts.

## Hosting the web demo

The real UI and slideshow over the bundled sample photos, hosted
(GitHub Pages, say) so the README can link it and anyone can try Raam
without building it.

## The golden suite

Every preset's `--exact` shot from the desktop host (every gallery page,
and every frame_ui page with each fixture, in both themes), pinned in
`tests/goldens.txt` as a hash of its pixels rather than as images; a
hash and a compare mode in the `raam` binary; and a script that checks
and blesses the set and, for a hash that moved, draws the shot at the
last blessed commit and now. A shot run is hermetic: a virtual clock
and no input from the machine, so neither load, nor the display's scale
or refresh rate, nor the mouse can move a hash. The suite runs
locally, not in CI, because Apple's GL and CI's software GL differ in
their pixels. The first bless is a commit of its own. The web build is
then diffed against the same shots by a script beside it, in headless
Chromium through a web preset mode that runs the same way.

## Smaller items

- A `--fullscreen` flag, so the desktop host can run a Linux frame.
- The App controller's effects are mapped to engine commands twice, in
  the Android and the desktop host; the mapping belongs in the engine.
- The desktop host has no sleep schedule or screen power yet, and no
  physical keyboard input.
- A steady-state memory reading on the frame, stills only, against the
  budget in [ARCHITECTURE.md](../ARCHITECTURE.md#memory-budget).

## Before v1

- WiFi setup and brightness in the settings.
- The release signing key, then the first public APK.

## The web demo, later

- "Try with Immich" against demo.immich.app.
- Settings kept in localStorage (the `Store` seam's web option).
- Weather.

## Experiments

- A first-generation Raspberry Pi as a stills-only frame. Its
  VideoCore IV has OpenGL ES 2, like the Mali-400, but the desktop host
  asks for desktop GL 3.2 under X or Wayland. A bare OS needs a KMS/DRM
  host with an EGL context for GLES2 and touch input, and Rust's ARMv6
  target. The question is how the renderer and the JPEG decoding fare
  on one 700 MHz core.
