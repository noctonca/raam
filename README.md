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

**Pre-alpha: design done, port in progress.** Everything Raam will do
has been proven on the real hardware across 28 experiments in a private
lab repo; the productisation into this repo is under way. Nothing here
runs a frame yet.

- The design: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- The route from experiments to product: [docs/plan/migration.md](docs/plan/migration.md)
- The coding discipline: [TIGERSTYLE.md](TIGERSTYLE.md)

## What it will be

- **Android is the product** — installed over the vendor app (root
  required for now), surviving low memory, decoder failures and GPU
  pressure by design.
- **The desktop build** is for development and doubles as a Linux
  frame (Raspberry Pi class).
- **The web build is the demo** — the real UI, compiled to wasm,
  pixel-matched to the device. To run it locally: `hosts/web/build.sh`,
  then serve `hosts/web/www` (e.g. `python3 -m http.server -d
  hosts/web/www`).

## Licence

MIT OR Apache-2.0, at your option.
