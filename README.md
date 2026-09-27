# Raam

*(rahm — Afrikaans for "frame")*

Turn a cheap Android photo frame into something you own: a fast, open
slideshow for your own photos — collages, Ken Burns, GPU transitions,
video clips, a clock — fed by [Immich](https://immich.app), a USB
stick, or photos uploaded from a phone.

Raam replaces the vendor app on frames like the SNUG 8″ (RK3126C,
Android 6, 493 MB RAM, GLES2-only GPU), and runs the same code as a
desktop app, on a Linux box, and in the browser.

## Status

**Pre-alpha.** Raam runs as the home app on the SNUG frame, as a
desktop app and as a browser demo, but there is no release yet: putting
it on a frame means building the APK yourself, and root.

- The design: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- What's next: [docs/plan/next.md](docs/plan/next.md)
- The coding discipline: [TIGERSTYLE.md](TIGERSTYLE.md)

## Running it

- **Desktop:** `cargo run --release` runs the slideshow over
  `~/Pictures/Raam` (or `--photos DIR`), with Immich set up in its
  settings; the mouse is a finger. `--page NAME` shows one screen of
  the UI instead. The flags are listed at the top of
  [src/main.rs](src/main.rs).
- **Web:** `hosts/web/build.sh`, then serve `hosts/web/www` (e.g.
  `python3 -m http.server -d hosts/web/www`).
- **Android:** `cd hosts/android && cargo apk2 build --release`, with
  the NDK set up; `.claude/env-check.sh` checks the machine first.

## What it will be

- **Android is the product** — installed over the vendor app (root
  required for now), surviving low memory, decoder failures and GPU
  pressure by design.
- **The desktop build** is for development and doubles as a Linux
  frame (Raspberry Pi class).
- **The web build is the demo** — the real UI, compiled to wasm,
  pixel-matched to the device.

## Licence

MIT OR Apache-2.0, at your option.
