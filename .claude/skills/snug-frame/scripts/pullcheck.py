#!/usr/bin/env python3
"""Checks one power-cut run's files (pulltest check collects them) and
prints its result row. Usage: pullcheck.py RUN_DIR, or --header.

What must hold after every cut: raam.db passes integrity_check, every
cached_file row has its file at the recorded size, no file is left
without a row (the startup sweep ran before the copy), the export parses,
and the new Raam got its pipeline up without a panic. The row also says
what became of the change or the write the cut was aimed at."""

import json
import os
import re
import sqlite3
import sys

COLUMNS = [
    "run", "case", "cut", "delay_ms", "stall", "boot_clock", "integrity",
    "rows", "strays", "sweep", "export", "change", "asset", "ready",
    "errors", "grace", "verdict",
]


def read(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read()
    except FileNotFoundError:
        return ""


def run_env(run):
    env = {}
    for line in read(os.path.join(run, "run.env")).splitlines():
        k, _, v = line.partition("=")
        env[k] = v
    return env


def on_disk(run):
    """(dir, name) -> size, from the stat listing taken on the frame."""
    files = {}
    for line in read(os.path.join(run, "files.txt")).splitlines():
        parts = line.split(" ", 2)
        if len(parts) == 3 and parts[1].isdigit():
            files[(parts[0], os.path.basename(parts[2]))] = int(parts[1])
    return files


def where(path):
    return (os.path.basename(os.path.dirname(path)), os.path.basename(path))


def aimed_change(before):
    """The last curation change the log saw before the cut."""
    last = None
    for m in re.finditer(
        r"db: (\S+) (?:Fill/Fit override -> (None|Some\((Fill|Fit)\))|(hidden|unhidden)) in",
        before,
    ):
        last = m
    if not last:
        return None
    key = last.group(1)
    if last.group(2):
        return key, "scale", (last.group(3) or "").lower() or None
    return key, "hidden", last.group(4) == "hidden"


def main():
    if sys.argv[1:] == ["--header"]:
        print("\t".join(COLUMNS))
        return
    run = sys.argv[1]
    env = run_env(run)
    before = read(os.path.join(run, "before.log"))
    boot = read(os.path.join(run, "boot.log"))
    grace = read(os.path.join(run, "grace.log"))
    out = dict.fromkeys(COLUMNS, "-")
    out.update(
        run=os.path.basename(run),
        case=env.get("case", "?"),
        cut=env.get("cut", "?"),
        delay_ms=env.get("delay_ms", "?"),
        stall=env.get("stall") or "-",
    )
    bad = []

    # The frame has no RTC: until NTP, a boot reads May 2021.
    m = re.search(r"^(\d\d-\d\d \d\d:\d\d:\d\d)", boot, re.M)
    out["boot_clock"] = m.group(1) if m else "?"

    conn = sqlite3.connect(os.path.join(run, "raam.db"))
    out["integrity"] = conn.execute("PRAGMA integrity_check").fetchone()[0]
    if out["integrity"] != "ok":
        bad.append("integrity")

    files = on_disk(run)
    rows = conn.execute("SELECT asset_id, path, bytes FROM cached_file").fetchall()
    wrong = [a for a, p, b in rows if files.get(where(p)) != b]
    out["rows"] = f"{len(rows) - len(wrong)}/{len(rows)}"
    if wrong:
        out["rows"] += " bad:" + ",".join(map(str, wrong[:5]))
        bad.append("rows")
    known = {where(p) for _, p, _ in rows}
    strays = [f"{d}/{n}" for (d, n) in files if (d, n) not in known]
    strays += [f"{d}/{n}=0" for (d, n), s in files.items() if s == 0]
    out["strays"] = ",".join(strays[:5]) if strays else "0"
    if strays:
        bad.append("strays")

    m = re.search(r"startup sweep dropped (\d+) rows with no file and (\d+) files with no row", boot)
    out["sweep"] = f"{m.group(1)}r/{m.group(2)}f" if m else "none"
    if not m:
        bad.append("sweep")

    curation = {
        k: (h == 1, s)
        for k, h, s in conn.execute("SELECT key, hidden, scale_mode FROM curation")
    }
    try:
        export = json.loads(read(os.path.join(run, "frame-curation.json")))
        items = {i["sha1"]: (bool(i["hidden"]), i["scale_mode"]) for i in export["items"]}
        differ = sorted(k for k in set(curation) | set(items) if curation.get(k) != items.get(k))
        out["export"] = "match" if not differ else f"differs:{len(differ)}"
    except (ValueError, KeyError, TypeError):
        out["export"] = "unreadable"
        bad.append("export")

    change = aimed_change(before)
    if change:
        key, field, want = change
        row = curation.get(key)
        have = None if row is None else (row[0] if field == "hidden" else row[1])
        out["change"] = f"{key[:8]}:{'kept' if have == want else 'lost'}"

    m = None
    for m in re.finditer(r"debug\.video\.stall: \w+ of asset (\d+) by", before):
        pass
    if m:
        asset = int(m.group(1))
        row = [p for a, p, _ in rows if a == asset]
        out["asset"] = f"{asset}:{'row+file' if row else 'neither'}"

    out["ready"] = "yes" if "EGL + pipeline + painter ready" in boot else "no"
    if out["ready"] != "yes":
        bad.append("ready")
    # Error lines are counted, not judged: the first seconds after a boot
    # log DNS failures until Wi-Fi is up. A panic or a failed export fails.
    out["errors"] = str(len(re.findall(r" E/raam", boot)))
    if re.search(r"panicked|curation export:", boot):
        bad.append("panic-or-export")

    if grace:
        slept = re.search(r"schedule: (sleeping[^\n]*)", grace)
        screen = read(os.path.join(run, "grace-screen.txt")).strip()
        out["grace"] = f"{screen or '?'}; {slept.group(1)[:60] if slept else 'no sleep logged'}"

    out["verdict"] = "PASS" if not bad else "FAIL:" + ",".join(bad)
    print("\t".join(out[c] for c in COLUMNS))


if __name__ == "__main__":
    main()
