# Next

What's planned, roughly in order. A larger item gets its own plan in
this directory when work on it starts.

## Smaller items

- A Raspberry Pi 4 or 5 as a Linux frame. Mesa's V3D driver offers
  OpenGL 3.1 at most, and OpenGL ES 3.1, so it needs the `gles`
  build, as a first-generation Pi does; untested there.
- Photos larger than the GPU's `GL_MAX_TEXTURE_SIZE`. A first-generation
  Pi's is 2048, and Immich's previews (short side 1440) of 3:2 and 16:9
  photos are 2160 to 2560 px long; nothing decodes them smaller yet.
- The desktop host has no sleep schedule or screen power yet.
  `--fullscreen` hides the pointer, so a mouse on a Linux frame has no
  cursor to aim with.
- The Android host reads no keys yet: a remote's D-pad or a USB
  keyboard on a frame, with the volume and system keys left to Android.
- Keys can't pick a collage's tile: a menu opened by key picks the
  first photo for Hide and Fill/Fit.
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

- A first-generation Raspberry Pi as a stills-only frame. The `gles`
  build runs on one under cage. Still open: how the renderer and the
  JPEG decoding fare on one 700 MHz core, what the compositor costs
  against a bare KMS/DRM host, and memory on 512 MB.
