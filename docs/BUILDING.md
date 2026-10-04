# Building Raam

Every build needs Rust 1.95 or newer. Raam is developed on macOS; CI
builds and tests it on Linux. Putting the APK on a frame is in the
[README](../README.md#putting-it-on-a-frame).

## Desktop

Nothing beyond Rust on macOS. On Linux the host links the system's
libGL, so it needs its development files too (`libgl-dev` on Debian
and Ubuntu):

```sh
cargo run --release -- --photos DIR
```

Add `--fullscreen` to fill the monitor, as a Linux frame runs it. The
flags are listed at the top of [src/main.rs](../src/main.rs).

`--features gles` makes the host ask for an OpenGL ES 2.0 context over
EGL instead of OpenGL 3.2, and link libGLESv2 instead of libGL
(`libgles-dev` on Debian and Ubuntu). It's for a GPU with no core
profile, such as a Raspberry Pi's.

### A Raspberry Pi

The host needs X11 or Wayland: on a Pi with no desktop, a kiosk
compositor such as cage runs it on its own. Started over SSH, cage
takes the screen through seatd (Debian's seatd service lets the `video`
group in):

```sh
LIBSEAT_BACKEND=seatd cage -- raam --fullscreen --photos DIR
```

A USB keyboard drives it with no touchscreen: ← and → go back and on,
any other key opens the menu, and the arrows, Enter and Escape find
their way through the settings ([UX.md](UX.md#keys)).

A first-generation Pi or a Pi Zero is ARMv6: Rust's
`arm-unknown-linux-gnueabihf` target, with a linker and C compiler
aimed at ARMv6 (for ring and SQLite). Debian's armhf cross toolchain
builds for ARMv7, which these can't run; clang and lld with a copy of
the Pi's own libraries and headers as the sysroot work.

### Wi-Fi

On Linux, Settings → Connectivity shows how the device is connected
and sets up Wi-Fi: it lists the networks nearby, joins one with its
password (or a hidden one by name), forgets one, and turns Wi-Fi off
and on. It talks to wpa_supplicant's control socket, so it needs no
root, but the device has to be set up for it. When something is
missing, the page says what.

- **wpa_supplicant runs for the adapter with a control socket, and may
  save what it's given.** Debian's `wpa_supplicant@wlan0` service (and
  Raspberry Pi OS before Bookworm) works this way. Its config,
  `/etc/wpa_supplicant/wpa_supplicant-wlan0.conf`, starts with:

  ```
  ctrl_interface=DIR=/run/wpa_supplicant GROUP=netdev
  update_config=1
  country=GB
  ```

  `country` is your two-letter country code; without it, 5 GHz
  channels stay off. Without `update_config=1`, a network joined from
  Raam is forgotten at the next restart, and the page says so.
- **Raam's user is in the socket's group:** `sudo usermod -aG netdev
  $USER`, then log in again.
- **The radio isn't blocked.** Raspberry Pi OS blocks every radio at
  boot until it's unblocked once (setting the Wi-Fi country in
  raspi-config does that), and a USB adapter plugged in later starts
  blocked again: `sudo rfkill unblock wifi`, once. Raam can see a
  block but not lift it.
- **Recommended:** wpa_supplicant writes its config with the
  service's umask, which leaves the saved Wi-Fi keys readable by every
  user. A drop-in keeps them private:

  ```sh
  sudo mkdir -p /etc/systemd/system/wpa_supplicant@.service.d
  printf '[Service]\nUMask=0077\n' | \
    sudo tee /etc/systemd/system/wpa_supplicant@.service.d/umask.conf
  sudo chmod 600 /etc/wpa_supplicant/wpa_supplicant-wlan0.conf
  sudo systemctl daemon-reload
  sudo systemctl restart wpa_supplicant@wlan0
  ```

What it doesn't do:

- NetworkManager, iwd or connman: where one of them runs Wi-Fi
  (Raspberry Pi OS from Bookworm on, most desktops), set Wi-Fi up with
  its own tools. The page says so.
- Enterprise (802.1X) and WEP networks: listed, but not joined.
- Turning the adapter off: Wi-Fi off stops wpa_supplicant using any
  network, and the adapter stays powered, since only root can switch
  the radio.
- Android and the web demo: not yet on Android; the demo has none.

On a first-generation Pi with a USB adapter, a scan of both bands
takes about 11 s, and a join about 6 s plus about 6 s for an address.
A wrong password shows after about 10 s, a network that isn't there
after about 25 s.

## Checks

The same six CI runs on every push and PR:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo clippy -p raam-web --target wasm32-unknown-unknown -- -D warnings
cargo clippy --workspace --all-targets --features gles -- -D warnings
cargo clippy -p raam-android --target armv7-linux-androideabi -- -D warnings
```

The last needs the NDK's clang for the C in the dependencies: set
`CC_armv7_linux_androideabi` to the NDK's
`toolchains/llvm/prebuilt/<host>/bin/armv7a-linux-androideabi23-clang`
and `AR_armv7_linux_androideabi` to `llvm-ar` in the same folder.

CI also runs Miri over the unsafe it can follow, on a pinned nightly
(the job names it; `rustup component add --toolchain <it> miri rust-src`):

```sh
cargo +nightly miri test -p raam-model
cargo +nightly miri test -p raam-core --lib gl::
```

That's the WebGL1 linkage's pointer helpers (`gl/raw.rs`, `gl/table.rs`)
and raam-model. Everything else unsafe is GL, NDK or libc calls, which
Miri can't follow. On Apple silicon add `--target x86_64-unknown-linux-gnu`
before running anything that draws text: Miri has no NEON, which egui's
glyph rasteriser uses there.

And cargo-deny over the dependency tree, by `deny.toml` (`brew install
cargo-deny` or `cargo install cargo-deny`):

```sh
cargo deny check
```

Pull requests also build the web demo ([below](#web)).

## Goldens

The desktop host's preset shots are the render-regression net: every
gallery page and every frame_ui page with each of its fixtures, in both
themes, plus a few that scroll, tap, type or use a smaller screen.
[tests/goldens.txt](../tests/goldens.txt) pins each one as a hash of its
pixels:

```sh
scripts/goldens.sh check            # or some of them: check 'set-albums*'
scripts/goldens.sh bless            # after a change meant to move pixels
scripts/goldens.sh show 'menu-*'    # before and after, and their diff
```

Pixels are exact per GPU and driver, so the hashes hold on the kind of
Mac the file's header names, and CI doesn't run them. The shots come
from the `quick` profile (release without LTO: the same pixels, and
about 7 s to relink after an edit where release takes about a minute),
one per core at a time; a whole check takes about 30 s. For a shot that
differs, the check draws it at the last blessed commit (built in a
worktree under `target/goldens/`) and now, with a diff that marks the
changed pixels in magenta. A change meant to move pixels re-blesses in
a commit of its own: `scripts/goldens-own-commit.sh` fails any commit
since `origin/main` (or the base it's given) that touches
`tests/goldens.txt` and anything else, and CI runs it over every pull
request.

The web build draws the same shots in headless Chromium through its
`?page=` mode, and `scripts/web-diff.py` compares them with the
desktop's, within 2 levels a channel: the browser's GL and the
desktop's differ in the last bit of blending on a few dozen pixels. It
needs [uv](https://docs.astral.sh/uv/) and the web build's tools below.

## Web

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <the one in Cargo.lock>
hosts/web/build.sh
python3 -m http.server -d hosts/web/www
```

`build.sh` prints the exact `cargo install` command when the installed
wasm-bindgen-cli differs from Cargo.lock's. It runs wasm-opt too when
binaryen is installed.

Every push to main builds the demo with the same script and deploys
`hosts/web/www` to [GitHub Pages](https://noctonca.github.io/raam/)
([.github/workflows/pages.yml](../.github/workflows/pages.yml)); a
pull request builds it without deploying.

## Android APK

You need the Android SDK with build-tools and platform 34 (set
`ANDROID_HOME` unless it's in Android Studio's default place), the NDK
(`ANDROID_NDK_ROOT` set to the versioned NDK folder), a JDK, and:

```sh
rustup target add armv7-linux-androideabi
cargo install cargo-apk2
export CARGO_APK_RELEASE_KEYSTORE=~/.android/debug.keystore
export CARGO_APK_RELEASE_KEYSTORE_PASSWORD=android
cd hosts/android && cargo apk2 build --release
```

The APK lands in `target/release/apk/raam-android.apk` at the repo
root.

- **Signing:** a release build must be signed; Android's debug keystore
  does until there are releases. Android Studio makes that file;
  otherwise `cargo apk2 build` once without `--release` creates it.
- **Your values** (the NDK path, the keystore) can live in an untracked
  `.env` at the repo root, which git ignores. Load it with
  `set -a; . ./.env; set +a`.
- **Checking the machine:** `scripts/env-check.sh` checks the NDK,
  cargo-apk2, signing and adb.
