#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["playwright==1.63.0"]
# ///
"""The web build against the golden suite's shots.

Every shot of scripts/goldens.sh that only names a page, a theme and a
backdrop is drawn twice: by the web host's ?page= mode (shot=1) in
headless Chromium, and by the desktop host. `raam --diff` then compares
them within a tolerance, because the browser's GL (ANGLE, over Metal on
a Mac) and the desktop's blend the last bit differently. The same
controller, painter and shaders draw both, so anything past that is a
per-host difference, which is a bug.

    scripts/web-diff.py [--tolerance N] [--no-build] [GLOB...]

Needs uv (which reads the dependency block above), the web build's
tools (docs/BUILDING.md) and Playwright's headless Chromium, which the
script names the install command for when it's missing. The pictures go
to target/goldens/web/: NAME.web.png, NAME.desktop.png and, for a shot
over the tolerance, NAME.diff.png.
"""

import argparse
import base64
import concurrent.futures
import functools
import http.server
import pathlib
import platform
import subprocess
import sys
import threading
import time

from playwright.sync_api import Error as PlaywrightError
from playwright.sync_api import sync_playwright

ROOT = pathlib.Path(
    subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    ).stdout.strip()
)
RAAM = ROOT / "target/release/raam"
OUT = ROOT / "target/goldens/web"
# A shot settles in a few passes, one an animation frame.
SETTLE_TIMEOUT_S = 10


def shots(globs):
    """(name, page, theme, backdrop) for each suite shot the web can draw."""
    listed = subprocess.run(
        [ROOT / "scripts/goldens.sh", "list", *globs], capture_output=True, text=True, check=True
    ).stdout
    out = []
    for line in listed.splitlines():
        name, *flags = line.split()
        opts = dict(zip(flags[::2], flags[1::2]))
        # A scroll, a tap or another size is desktop-only tooling.
        if len(flags) == 6 and set(opts) == {"--page", "--theme", "--backdrop"}:
            out.append((name, opts["--page"], opts["--theme"], opts["--backdrop"]))
    return out


def desktop(name, page, theme, backdrop):
    png = OUT / f"{name}.desktop.png"
    subprocess.run(
        [RAAM, "--page", page, "--theme", theme, "--backdrop", backdrop, "--exact",
         "--screenshot", png],
        capture_output=True, check=True, timeout=60,
    )
    return png


def serve(root):
    """hosts/web/www on a free local port, for as long as the script runs."""
    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass

    handler = functools.partial(Quiet, directory=root)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f"http://127.0.0.1:{server.server_address[1]}"


def web(browser_page, base, name, page, theme, backdrop):
    browser_page.goto(f"{base}/?page={page}&theme={theme}&backdrop={backdrop}&shot=1")
    deadline = time.monotonic() + SETTLE_TIMEOUT_S
    while True:
        try:
            st = browser_page.evaluate(
                "async () => JSON.parse((await import('./pkg/raam_web.js')).status())"
            )
        except PlaywrightError as e:  # the module is still starting
            st = {"error": e.message}
        if st.get("idle"):
            break
        if time.monotonic() > deadline:
            raise RuntimeError(f"{name}: never settled ({st})")
        time.sleep(0.05)
    url = browser_page.evaluate("document.getElementById('frame').toDataURL('image/png')")
    png = OUT / f"{name}.web.png"
    png.write_bytes(base64.b64decode(url.split(",", 1)[1]))
    return png


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--tolerance", type=int, default=2,
                    help="levels a channel may differ by (default 2)")
    ap.add_argument("--no-build", action="store_true",
                    help="use the web and desktop builds as they are")
    ap.add_argument("globs", nargs="*", help="shot names, as scripts/goldens.sh list prints them")
    args = ap.parse_args()

    if not args.no_build:
        subprocess.run([ROOT / "hosts/web/build.sh"], check=True, stdout=subprocess.DEVNULL)
        subprocess.run(["cargo", "build", "--release", "--bin", "raam", "-q"], cwd=ROOT, check=True)
    todo = shots(args.globs)
    if not todo:
        sys.exit(f"no shot matches {' '.join(args.globs)}")
    OUT.mkdir(parents=True, exist_ok=True)
    base = serve(ROOT / "hosts/web/www")
    # Real WebGL on the GPU; Chromium's default on a Mac is a software one.
    gpu = ["--use-angle=metal", "--enable-gpu", "--ignore-gpu-blocklist"]
    launch_args = gpu if platform.system() == "Darwin" else []
    print(f"{len(todo)} shots, tolerance {args.tolerance} levels")

    failed = []
    with concurrent.futures.ThreadPoolExecutor(4) as pool, sync_playwright() as pw:
        drawn = {s[0]: pool.submit(desktop, *s) for s in todo}
        try:
            browser = pw.chromium.launch(args=launch_args)
        except PlaywrightError as e:
            if "Executable doesn't exist" not in e.message:
                raise
            sys.exit("no headless Chromium for this Playwright; install it with\n"
                     "  uv run --with playwright==1.63.0 python -m playwright install "
                     "chromium-headless-shell")
        tab = browser.new_page(viewport={"width": 1280, "height": 800}, device_scale_factor=1)
        for shot in todo:
            name = shot[0]
            w = web(tab, base, *shot)
            d = drawn[name].result()
            diff = OUT / f"{name}.diff.png"
            r = subprocess.run(
                [RAAM, "--diff", d, w, "--tolerance", str(args.tolerance), "--out", diff],
                capture_output=True, text=True, check=False,
            )
            if r.returncode == 0:
                diff.unlink(missing_ok=True)
            else:
                failed.append(name)
            print(f"  {'ok  ' if r.returncode == 0 else 'DIFF'} {name}: {r.stdout.strip()}",
                  flush=True)
        browser.close()
    if failed:
        print(f"{len(failed)} of {len(todo)} differ past {args.tolerance} levels; "
              f"see {OUT.relative_to(ROOT)}/*.diff.png")
        sys.exit(1)
    print(f"all {len(todo)} within {args.tolerance} levels")


if __name__ == "__main__":
    main()
