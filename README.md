# Raam

[![CI](https://github.com/noctonca/raam/actions/workflows/ci.yml/badge.svg)](https://github.com/noctonca/raam/actions/workflows/ci.yml)

*(rahm — Afrikaans for "frame")*

Raam replaces the vendor app on a cheap Android photo frame with a fast
slideshow of your own photos, straight from your
[Immich](https://immich.app) server. No vendor account, no analytics.

**Pre-alpha.** It runs as the home app on one frame, a SNUG 10.1″, and
there is no release yet. Today you need a computer that can build the
APK, and a frame with root. If that's not you, watch for a release.

## What it does

- **Photos from Immich albums** you pick on the frame, or from a folder
  on the frame. 1 GB is cached on the frame by default (a setting, up
  to 4 GB), so the slideshow keeps going when the server is away.
- **Collages** of two to four photos, **Ken Burns** aimed at the faces
  Immich found, and five **GPU transitions**, one of your choice or all
  in turn.
- **Video clips**, with sound if you want it.
- **A clock** in two styles and any corner, with the local weather if
  you turn it on.
- **From the frame:** hide a photo (with undo), switch it between
  filling the screen and fitting it, and change every setting on the
  touchscreen, the server address and API key included.
- **A sleep schedule** that turns the screen off, 23:00–05:00 by default.

## How it works

Raam draws everything itself with OpenGL ES 2 shaders on the frame's
GPU: the collages, the blurred backgrounds, Ken Burns, the transitions
and the clock. Video goes from the frame's hardware decoder straight
into a GPU texture. One Rust core, with its own design system on egui,
draws the slideshow and the UI on the frame, on a desktop and in a
browser, so you can try it on a computer first. It has no Java or
Kotlin of its own; Android's parts (the window, touch, video decoding
and power) live in one host crate.

## Compared with Frameo

Frameo is the app these frames ship with.

| | Frameo | Raam |
|---|---|---|
| Photos come from | Friends' phones, through the Frameo app and Frameo's cloud (end-to-end encrypted); SD card or USB | Albums on your Immich server; a folder on the frame |
| Account | The phone app needs a Frameo account | None: an Immich API key |
| Videos | Up to 15 s from a phone, 2 min with Frameo+ | No length limit (an Immich clip must download within 5 minutes); H.264 up to 1080p |
| Analytics | Usage analytics and diagnostic logs (the analytics can be switched off) | None |
| Memory in use* | about 110 MB | about 45 MB |
| App size | 74 MB | 5 MB |

\* App plus GPU memory while showing photos, measured the same way on
the SNUG (Frameo 1.32.12).

**What you give up:** friends and family sending photos from their
phones (they add them to your Immich album instead; the frame checks
for new photos every 30 minutes), captions and reactions, Wi-Fi and
brightness settings, and updates without a computer.

**What Raam sends where:** photos and clips come from your Immich
server, and nothing else goes out unless you turn on the weather
(Settings → Display; off by default). Then Raam looks up the frame's
location from its IP address once per start (ipwho.is, or ipapi.co),
and asks [Open-Meteo](https://open-meteo.com) for the weather every 15
minutes while the clock is on the screen.

**Why not ImmichFrame or Immich Kiosk?** Immich Kiosk needs its own
server and a browser from 2022 or later (Chrome 106+); the SNUG's
WebView is Chromium 44, from 2015.
ImmichFrame's Android app has a reduced mode for such frames and needs
an ImmichFrame server. Raam draws with OpenGL ES 2 and talks to Immich
directly.

## Will it run on my frame?

- Android 6 (API 23). Newer versions are untested.
- A CPU that runs 32-bit ARM apps (armeabi-v7a).
- OpenGL ES 2.0.
- Root (`su`): Raam uses it to turn the screen off and to reach the
  photos folder, and switching Frameo off needs it too.
- Tested on one frame: the SNUG SNFRM-8GB10-BK (10.1″, 1280×800,
  RK3126, Mali-400 MP2, 493 MB RAM). 1024×600, the common 7″ panel, is
  designed for but untested.

With ADB on (step 1 below), `adb shell getprop ro.build.version.release`
gives the Android version, `adb shell getprop ro.product.cpu.abilist`
must include `armeabi-v7a`, and `adb shell su -c id` must answer
`uid=0(root)`.

## Putting it on a frame

1. **Turn on ADB** in Frameo: Settings → About → Beta program, then
   ADB access ([Frameo's guide](https://support.frameo.com/hc/en-us/articles/6126183308434--How-to-Enable-ADB-on-Your-Frame),
   which warns you do this at your own risk). This also joins Frameo's
   beta program. Connect the frame to your computer by USB.
2. **Set Wi-Fi and brightness in Frameo now.** Raam has no settings for
   them yet; they stay as Frameo leaves them.
3. **Try it**, with Frameo still in place: build the APK
   ([docs/BUILDING.md](docs/BUILDING.md#android-apk)), then from the
   repo root:

   ```sh
   adb install target/release/apk/raam-android.apk
   adb shell am start -n io.github.noctonca.raam/android.app.NativeActivity
   ```

   Tap for the menu. Enter your Immich address and API key under
   **Settings → Server** (they apply when the menu closes), then pick
   albums under **Settings → Photos → Albums**. Photos in
   `/sdcard/Pictures/Frame` show too. Until step 4, Frameo stays in
   charge: its sleep schedule takes the screen back at bedtime, and
   after a reboot the frame may ask which home app to use.
4. **Make it the home app:**
   `adb shell su -c 'pm disable-user --user 0 net.frameo.frame'`, wait
   ten seconds (a sooner reboot loses the change), then `adb reboot`.
   The frame now starts Raam.

**Going back:** `adb shell su -c 'pm enable net.frameo.frame'`, then
`adb uninstall io.github.noctonca.raam`, wait ten seconds, and
`adb reboot`. Disabling Frameo keeps its photos and settings, and
nothing here touches the system partition.

## Running it on a computer

Every build needs Rust 1.95 or newer;
[docs/BUILDING.md](docs/BUILDING.md) has what else each one needs.

- **Desktop** (macOS; Linux untested):
  `cargo run --release -- --photos DIR` runs the slideshow over a
  folder (by default `~/Pictures/Raam`); Immich is set up in its
  settings, and the mouse is a finger.
  `--size 1024x600` tries the 7″ panel. Clips don't play on the desktop.
  `--page NAME` shows one screen of the UI instead; every flag is listed
  at the top of [src/main.rs](src/main.rs). A Linux frame (a Raspberry
  Pi, say) is planned, not tested.
- **Web:** `hosts/web/build.sh`, then serve `hosts/web/www` (e.g.
  `python3 -m http.server -d hosts/web/www`). It shows twenty bundled
  sample photos, with no clips and no weather, and forgets its settings
  on reload.

## For developers

A Cargo workspace: `raam-model` (types and every named limit),
`raam-core` (the egui UI and a hand-written GLES2 renderer; synchronous,
no I/O, also built for wasm), `raam-engine` (SQLite, HTTP, worker
threads), and three hosts: Android, desktop and web. The same drawing
code runs on all three. No wgpu, glow or async.

`cargo test --workspace` runs the tests; CI also runs fmt and clippy
(`-D warnings`, native and wasm).

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): the design, the seams
  and the dependency register
- [docs/BUILDING.md](docs/BUILDING.md): building every host, and the
  checks CI runs
- [docs/plan/next.md](docs/plan/next.md): what's next
- [CONTRIBUTING.md](CONTRIBUTING.md): how to contribute; AI-assisted
  work is welcome, with the same bar
- [AGENTS.md](AGENTS.md): the rules, for people and their assistants
- [TIGERSTYLE.md](TIGERSTYLE.md): the coding discipline
- [docs/UX.md](docs/UX.md): the UI rules
- [Issues](https://github.com/noctonca/raam/issues)

## Credits

Raam is not affiliated with Immich, Frameo or ImmichFrame. It builds on
[gl-transitions](https://github.com/gl-transitions/gl-transitions) (MIT),
[egui](https://github.com/emilk/egui) and
[egui_keyboard](https://github.com/podusowski/egui_keyboard), the
Roboto (SIL OFL 1.1) and Material Symbols (Apache-2.0) fonts, and
weather data by [Open-Meteo.com](https://open-meteo.com) (CC BY 4.0).
The Detailed clock follows ImmichFrame's. The web demo's sample photos
are CC0, from Wikimedia Commons.

## Licence

MIT OR Apache-2.0, at your option.
