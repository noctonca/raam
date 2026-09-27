"""Visual QA tools for the frame's egui UI.

Run with Pillow on hand, no venv needed:
    uv run --with pillow python .claude/skills/ux-qa/scripts/qa.py <command> ...

Commands (coordinates are device pixels, 1 px = 1 dp on the frame):
  shoot OUT [--bin B] [--pages a,b] [--themes dark,light] [--scrolls 0,600]
            [--tall H] [--extra "ARGS"]
      Screenshot every page x theme x scroll with a desktop host into OUT as
      <page>-<theme>-<scroll>.png (or <page>-<theme>-tall.png with --tall,
      a 1280xH window so a whole page fits in one capture; H is capped
      near a laptop screen's height, about 1900).
  frame OUT --prop PREFIX [--pages a,b] [--themes dark,light] [--wait S]
      The same set captured on the device over adb by setting
      PREFIX.page / PREFIX.theme, as
      <page>-<theme>-0.png so `diff` can pair it with a desktop shot.
  ink IMG X0 X1 Y0 Y1 [--bg X,Y] [--thresh N]
      Ink extents in the box: pixels differing from the background (sampled
      at X0,Y0 unless --bg). Prints y/x ranges and centres.
  glyphs IMG X0 X1 Y0 Y1 [N]
      The first N glyph runs (contiguous ink columns): each one's x range,
      y range and y centre. For "is the text centred on its icon".
  zoom IMG OUT X Y W H [K]
      Crop and enlarge K times (default 3), nearest neighbour, for joins.
  contrast A B
      WCAG contrast ratio. A and B are hex colours (#RRGGBB) or IMG@X,Y.
  sections IMG... [--edge 400] [--pane 384,1240]
      For each section header (saturated text 8-15 px tall starting on the
      content edge): the ink gap above it, from its baseline to the next ink
      below, and the largest ink gap inside its group. Proximity holds when
      every group's inside gap is smaller than the gap to the next header.
  diff DIR_A DIR_B
      Pair PNGs by name; per pair: bounding box of changed pixels, count, the
      largest per-channel difference and how many pixels differ by 24 or more
      levels. Frame vs desktop: a max of about 13 is rasterisation only.
"""
import os
import subprocess
import sys
import time

from PIL import Image, ImageChops

DESKTOP = "target/release/raam"


def dist(a, b):
    return sum(abs(i - j) for i, j in zip(a[:3], b[:3]))


def opts(args, defaults):
    """Split `--key value` pairs off args; returns (positional, options)."""
    pos, o, i = [], dict(defaults), 0
    while i < len(args):
        if args[i].startswith("--"):
            o[args[i][2:]] = args[i + 1]
            i += 2
        else:
            pos.append(args[i])
            i += 1
    return pos, o


def shoot(args):
    pos, o = opts(args, {"bin": DESKTOP, "pages": "settings,components,colours,type,icons,targets,probe",
                         "themes": "dark,light", "scrolls": "0,600,1200", "tall": "", "extra": ""})
    out = pos[0]
    os.makedirs(out, exist_ok=True)
    for page in o["pages"].split(","):
        for theme in o["themes"].split(","):
            runs = [("tall", ["--exact", "--size", f"1280x{o['tall']}"])] if o["tall"] else \
                   [(s, ["--exact", "--scroll", s]) for s in o["scrolls"].split(",")]
            for tag, a in runs:
                path = os.path.join(out, f"{page}-{theme}-{tag}.png")
                cmd = [o["bin"], "--theme", theme, "--page", page, *a, *o["extra"].split(), "--screenshot", path]
                r = subprocess.run(cmd, capture_output=True, text=True)
                print(("ok  " if r.returncode == 0 else "FAIL") + " " + path)


def frame(args):
    pos, o = opts(args, {"prop": "", "pages": "settings,components,colours,type,icons,targets,probe",
                         "themes": "dark,light", "wait": "1.5"})
    if not o["prop"]:
        sys.exit("frame needs --prop PREFIX (raam has no page props yet: put the frame on the page by hand and use `adb exec-out screencap -p` directly)")
    out = pos[0]
    os.makedirs(out, exist_ok=True)
    for theme in o["themes"].split(","):
        subprocess.run(["adb", "shell", "setprop", f"{o['prop']}.theme", theme], check=True)
        for page in o["pages"].split(","):
            subprocess.run(["adb", "shell", "setprop", f"{o['prop']}.page", page], check=True)
            time.sleep(float(o["wait"]))
            path = os.path.join(out, f"{page}-{theme}-0.png")
            # exec-out, not `shell screencap`: the shell's tty mangles the PNG.
            with open(path, "wb") as f:
                subprocess.run(["adb", "exec-out", "screencap", "-p"], stdout=f, check=True)
            print("ok   " + path)


def ink_box(img, x0, x1, y0, y1, bg, thresh):
    px = img.load()
    ys = [y for y in range(y0, y1) if any(dist(px[x, y], bg) > thresh for x in range(x0, x1))]
    xs = [x for x in range(x0, x1) if any(dist(px[x, y], bg) > thresh for y in range(y0, y1))]
    return xs, ys


def ink(args):
    pos, o = opts(args, {"bg": "", "thresh": "60"})
    img = Image.open(pos[0]).convert("RGB")
    x0, x1, y0, y1 = map(int, pos[1:5])
    bg = img.getpixel(tuple(map(int, o["bg"].split(",")))) if o["bg"] else img.getpixel((x0, y0))
    xs, ys = ink_box(img, x0, x1, y0, y1, bg, int(o["thresh"]))
    if not ys:
        print("no ink")
        return
    print(f"y {ys[0]}..{ys[-1]} (centre {(ys[0] + ys[-1] + 1) / 2})  x {xs[0]}..{xs[-1]} (centre {(xs[0] + xs[-1] + 1) / 2})")


def glyphs(args):
    img = Image.open(args[0]).convert("RGB")
    x0, x1, y0, y1 = map(int, args[1:5])
    n = int(args[5]) if len(args) > 5 else 1
    px, bg = img.load(), img.getpixel((x0, y0))
    cols = [x for x in range(x0, x1) if any(dist(px[x, y], bg) > 60 for y in range(y0, y1))]
    runs = []
    for x in cols:
        if runs and x == runs[-1][1] + 1:
            runs[-1][1] = x
        else:
            runs.append([x, x])
    for a, b in runs[:n]:
        ys = [y for y in range(y0, y1) if any(dist(px[x, y], bg) > 60 for x in range(a, b + 1))]
        print(f"x {a}..{b}  y {ys[0]}..{ys[-1]}  centre {(ys[0] + ys[-1] + 1) / 2}")


def zoom(args):
    src, out = args[0], args[1]
    x, y, w, h = map(int, args[2:6])
    k = int(args[6]) if len(args) > 6 else 3
    Image.open(src).crop((x, y, x + w, y + h)).resize((w * k, h * k), Image.NEAREST).save(out)
    print(out)


def colour(spec):
    if "@" in spec:
        path, xy = spec.split("@")
        return Image.open(path).convert("RGB").getpixel(tuple(map(int, xy.split(","))))
    h = spec.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def luminance(c):
    c = [v / 255 for v in c]
    c = [v / 12.92 if v <= 0.03928 else ((v + 0.055) / 1.055) ** 2.4 for v in c]
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]


def contrast(args):
    a, b = sorted((luminance(colour(args[0])), luminance(colour(args[1]))), reverse=True)
    r = (a + 0.05) / (b + 0.05)
    verdict = "body ok" if r >= 4.5 else "large text / icons only" if r >= 3 else "FAILS"
    print(f"{r:.2f}:1  {verdict}")


def sections(args):
    pos, o = opts(args, {"edge": "400", "pane": "384,1240"})
    edge = int(o["edge"])
    left, right = map(int, o["pane"].split(","))
    for path in pos:
        img = Image.open(path).convert("RGB")
        px, (w, h) = img.load(), img.size
        bg = px[min(right + 10, w - 1), h - 20]
        is_ink = lambda x, y: dist(px[x, y], bg) > 12
        rows = [y for y in range(66, h - 4) if any(is_ink(x, y) for x in range(left, right))]
        runs = []
        for y in rows:
            if runs and y == runs[-1][1] + 1:
                runs[-1][1] = y
            else:
                runs.append([y, y])
        heads = []
        for i, (a, b) in enumerate(runs):
            if not 8 <= b - a + 1 <= 15:
                continue
            xs = [x for x in range(left, right) if any(is_ink(x, y) for y in range(a, b + 1))]
            if xs[0] not in (edge, edge + 1):
                continue
            pts = [px[x, y] for x in xs for y in range(a, b + 1) if is_ink(x, y)]
            if sum(1 for p in pts if max(p) - min(p) > 40) / len(pts) < 0.5:
                continue  # not saturated: body text, not a header
            first = [x for x in xs if x < xs[0] + 6]
            base = max(y for y in range(a, b + 1) for x in first if is_ink(x, y))
            above = a - runs[i - 1][1] - 1 if i > 0 else None
            below = runs[i + 1][0] - base - 1 if i + 1 < len(runs) else None
            heads.append((a, above, below, i))
        print(os.path.basename(path))
        for k, (a, above, below, i) in enumerate(heads):
            end = heads[k + 1][3] if k + 1 < len(heads) else len(runs)
            inside = [runs[j][0] - runs[j - 1][1] - 1 for j in range(i + 2, end)]
            nxt = heads[k + 1][1] if k + 1 < len(heads) else None
            mx = max(inside) if inside else None
            flag = "  PROXIMITY" if mx is not None and nxt is not None and mx >= nxt else ""
            print(f"  header y{a}: above {above}, below {below}, largest inside {mx}{flag}")


def diff(args):
    a_dir, b_dir = args[0], args[1]
    for name in sorted(os.listdir(a_dir)):
        pb = os.path.join(b_dir, name)
        if not name.endswith(".png") or not os.path.exists(pb):
            continue
        a = Image.open(os.path.join(a_dir, name)).convert("RGB")
        b = Image.open(pb).convert("RGB")
        if a.size != b.size:
            print(f"{name}: sizes differ {a.size} vs {b.size}")
            continue
        d = ImageChops.difference(a, b)
        box = d.getbbox()
        if not box:
            print(f"{name}: identical")
            continue
        hist = d.convert("L").histogram()
        top = max(i for i, v in enumerate(hist) if v)
        print(f"{name}: box {box}, {sum(hist[1:])} px differ, max {top}, >=24: {sum(hist[24:])}")


COMMANDS = {"shoot": shoot, "frame": frame, "ink": ink, "glyphs": glyphs, "zoom": zoom,
            "contrast": contrast, "sections": sections, "diff": diff}

if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] not in COMMANDS:
        sys.exit(__doc__)
    COMMANDS[sys.argv[1]](sys.argv[2:])
