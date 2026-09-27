#!/usr/bin/env bash
# Preflight for building and deploying Raam from a dev machine. Personal
# values (a frame's adb serial or address, keystore paths) are never
# committed — this repo is public; they live in the machine-local,
# untracked .env at the repo root, or in the environment. Hard
# requirements FAIL; conveniences warn.
#
#   scripts/env-check.sh     (from the repo root; a worktree reads the
#                            main checkout's .env)
#
#   ANDROID_NDK_ROOT                      the VERSIONED NDK dir
#                                         (cargo-apk2 rejects ndk/ itself)
#   CARGO_APK_RELEASE_KEYSTORE            signing for `cargo apk2 build
#   CARGO_APK_RELEASE_KEYSTORE_PASSWORD     --release` (the dev debug
#                                         keystore until the release key)
#   RAAM_FRAME_SERIAL (optional)          the target frame, so adb
#                                         reachability is checked
#   RAAM_CURATION_JSON (optional)         a curation export to import on
#                                         a fresh install (engine db.rs)
set -uo pipefail

ENVF="$(git rev-parse --git-common-dir)/../.env"
if [ -f "$ENVF" ]; then
    set -a
    # shellcheck disable=SC1090
    . "$ENVF"
    set +a
fi

fail=0
ok()   { printf '  ok       %s\n' "$1"; }
bad()  { printf '  FAIL     %-24s %s\n' "$1" "$2"; fail=1; }
warn() { printf '  warn     %-24s %s\n' "$1" "$2"; }

if [ -z "${ANDROID_NDK_ROOT:-}" ]; then
    bad ANDROID_NDK_ROOT "not set"
elif [ ! -d "$ANDROID_NDK_ROOT" ]; then
    bad ANDROID_NDK_ROOT "does not exist: $ANDROID_NDK_ROOT"
elif [[ "$(basename "$ANDROID_NDK_ROOT")" != [0-9]*.* ]]; then
    bad ANDROID_NDK_ROOT "must be the versioned directory (…/ndk/<version>): $ANDROID_NDK_ROOT"
else
    ok ANDROID_NDK_ROOT
fi

if command -v cargo-apk2 >/dev/null 2>&1; then
    ok cargo-apk2
else
    bad cargo-apk2 "not installed (cargo install cargo-apk2)"
fi

if [ -z "${CARGO_APK_RELEASE_KEYSTORE:-}" ] || [ -z "${CARGO_APK_RELEASE_KEYSTORE_PASSWORD:-}" ]; then
    warn signing "CARGO_APK_RELEASE_KEYSTORE(_PASSWORD) not set; a --release build will not sign"
elif [ ! -f "$CARGO_APK_RELEASE_KEYSTORE" ]; then
    warn signing "keystore not found: $CARGO_APK_RELEASE_KEYSTORE"
else
    ok signing
fi

if command -v adb >/dev/null 2>&1; then
    if [ -n "${RAAM_FRAME_SERIAL:-}" ]; then
        if adb devices | grep -q "^${RAAM_FRAME_SERIAL}[[:space:]]"; then
            ok frame
        else
            warn frame "$RAAM_FRAME_SERIAL not in 'adb devices' (adb connect, or USB)"
        fi
    fi
else
    warn adb "not on PATH; no device work from here"
fi

if [ -n "${RAAM_CURATION_JSON:-}" ] && [ ! -f "$RAAM_CURATION_JSON" ]; then
    warn curation "RAAM_CURATION_JSON points at nothing: $RAAM_CURATION_JSON"
fi

[ "$fail" -eq 0 ] && echo "env-check: ok"
exit "$fail"
