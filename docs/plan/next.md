# Next

What's planned, roughly in order. A larger item gets its own plan in
this directory when work on it starts.

## Smaller items

- A Raspberry Pi 4 or 5 as a Linux frame. Mesa's V3D driver offers
  OpenGL 3.1 at most, and OpenGL ES 3.1; the desktop host asks for
  OpenGL 3.2, so there it needs a GLES context, as on the frame.
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
