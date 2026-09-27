# Building Raam

Every build needs Rust 1.95 or newer. Raam is developed on macOS; CI
builds and tests it on Linux. Putting the APK on a frame is in the
[README](../README.md#putting-it-on-a-frame).

## Desktop

Nothing beyond Rust:

```sh
cargo run --release -- --photos DIR
```

The flags are listed at the top of [src/main.rs](../src/main.rs).

## Checks

The same four CI runs on every push and PR:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo clippy -p raam-web --target wasm32-unknown-unknown -- -D warnings
```

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
