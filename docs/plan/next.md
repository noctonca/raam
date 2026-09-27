# Next

What's planned, roughly in order. A larger item gets its own plan in
this directory when work on it starts.

## The golden suite

Every preset's `--exact` shot, in both themes, checked in; a compare
mode in the `raam` binary; and a script that blesses and checks the set.
The suite runs locally, not in CI, because Apple's GL and CI's software
GL differ in their pixels. It also pins the display scale: two presets
(the server page with the keyboard up, and the albums page scrolled)
come out differently in a scale-1 and a scale-2 window. The first bless
is a commit of its own. The web build is then diffed against the suite,
by a script checked in beside it.

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
- Hosting the demo.
