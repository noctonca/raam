# Power-cut tests on the frame

Instrumentation for the open half of
[#17](https://github.com/noctonca/raam/issues/17): cutting the power at
a chosen moment instead of by luck, and checking what survived. Pulling
the plug by hand at a random moment, with 240 photos in rotation, can't
hit a window of a few milliseconds, and can't be repeated.

## What's being tested

Each case names the window, how the cut is aimed at it, and what must
hold after the boot.

| Case | Window | Aimed by | Expected after boot |
|---|---|---|---|
| C1 | curation commit done, export not yet written | `stall=export` | DB has the change; the export is one change behind (see the open question below) |
| C2 | just after a curation commit, no stall | cut 0, 50, 200, 1000 ms after the commit's log line, 5 runs each | the change survives, or is lost from both DB and export, never torn |
| C3 | preview temp file synced, not renamed | `stall=rename` | the sweep deletes the temp file; no row |
| C4 | preview renamed, row not committed | `stall=row` | the sweep deletes the file; no row |
| C5 | on-demand fetch and prefetch storing the same preview | `stall=rename` on the prefetch, then the same photo requested on screen | one whole file and its row, or neither |
| C6 | the 600 s sleep grace after a boot with the wrong clock | any cut, then 11 min untouched | the screen stays on once NTP fixes the clock; the log says why |

In every case: `PRAGMA integrity_check` is ok, every `cached_file` row
has its file at the recorded size, no `.tmp` or `.part` file outlives the
sweep, the export parses, and Raam reaches `EGL + pipeline + painter
ready` with no panic.

## The cut

Two ways, decided 2026-10-03:

- **sysrq, for the scripted runs.** `echo b > /proc/sysrq-trigger` as
  root reboots at once, without syncing or unmounting
  (`/proc/sys/kernel/sysrq` is 1 on the frame). Whatever is still in the
  page cache is lost, as in a power cut. It is gentler in one way: the
  eMMC stays powered, so its own write cache isn't lost. Hence the real
  pulls below.
- **Real pulls, to confirm.** C2 at 0 ms, C3 and C4, repeated with the
  plug: the script stalls, says "pull now" on the Mac (`say`), and the
  maintainer pulls. A 30 s stall leaves room for a human's reaction.

The trigger runs on the frame, not over adb: a root script pushed to
`/data/local/tmp` follows `logcat -s raam` and writes to
`sysrq-trigger` when the marker line appears, after an optional delay.
Over Wi-Fi adb, the round trip alone is tens of milliseconds and varies.
It runs inside an adb session the Mac holds open, because on the frame
nothing started in the background outlives its adb session.

## A known photo on screen

C1 to C5 need the photo on screen, or the one being fetched, to be
known. The script:

1. generates 8 numbered JPEGs on the Mac (a big digit on a plain
   colour, so a screenshot shows which is up) and pushes them to
   `/sdcard/Pictures/Frame/pulltest/`;
2. sets `debug.video.only_path=pulltest`, so the queue holds only them;
3. force-stops Raam, so the local scan and the queue reload at once
   instead of within the 300 s scan interval.

Curation changes are scripted taps: open the menu, press Fill/Fit or
Hide. The menu's buttons are the same for each test photo, so the
positions are measured once from a screenshot and kept in the script.
No intent or broadcast path is added for this.

C5 needs an Immich photo instead. The script deletes one cached
preview as root and force-stops Raam, so the sweep drops its row and
prefetch starts. To be checked first: whether an Immich item's
`location` carries a path `only_path` can match. If not, C5 aims with
`stall=rename` alone and takes whichever preview is stalled.

## The stall points

A test-only `debug.video.stall=<site>:<seconds>` prop, read like the
other `debug.video.*` switches. Off unless set. On a match, the thread
logs `debug.video.stall: <site> for <n>s` (the marker the trigger
waits for), sleeps, and carries on.

| Site | Where | Thread |
|---|---|---|
| `export` | after `db::set_scale`/`set_hidden` commits, before the export's `write_atomic` | writer |
| `rename` | in `write_atomic`, after the temp file is synced, before the rename | any |
| `row` | after a preview's rename and directory sync, before `db::insert_cached` | library, fetch |

`rename` takes an optional thread filter (`rename@prefetch`,
`rename@fetch`) for C5. The longest stall is a named limit in
`limits.rs`. The switch reaches `write_atomic` by being passed in, not
by a global. A unit test covers that an unset or malformed prop never
stalls. The props table in the snug-frame skill gets the new row.

## The check after each boot

`pulltest check` in the snug-frame skill, from the method used on
2026-10-03:

1. wait for a new `boot_id`, then save `logcat -s raam` from the boot
   (the ring holds only a few minutes);
2. in one root script: force-stop Raam, copy `raam.db`, `-wal`, `-shm`
   and the listings of `files/immich-cache` and `files/local-previews`
   to a scratch folder, then start Raam again;
3. locally: `integrity_check`, rows against file sizes, stray `.tmp` or
   `.part` files, the export parsed against the curation table, and the
   startup sweep's line;
4. print one result row (case, delay, verdict, counts) for #17's table.

C6 skips step 2 for 11 minutes, because stopping Raam resets the grace
timer. It follows the log instead and records the clock at boot, when
NTP fixed it, and whether the screen slept.

## Order of work

1. **PR 1, the stall points:** `debug.video.stall` in `raam-engine`, the
   limit, the test, the skill's props table.
2. **PR 2, the harness:** `scripts/pulltest` in the snug-frame skill
   (`setup`, `arm`, `curate`, `evict`, `check`, `teardown`), with a
   section in `references/raam-checks.md`. `teardown` removes the test photos and
   the scratch, clears the props, and reverts any curation the run made.
   Nothing stays on the frame, so `docs/FRAME-SETUP.md` doesn't change.
3. **The runs:** C1 to C6 with sysrq, then the real pulls. The results
   go on #17 as a table, like the first three pulls.
4. **Clean-up:** revert the Fit override left by the 2026-10-03 pull on
   the Immich sunset photo.

## Open questions

- **Is a stale export acceptable?** After C1 the DB is one change ahead
  of the export, and nothing writes the export again until the next
  change. The import runs only into an empty curation table, so the
  export is a backup, not the source; a stale backup loses the last
  change only if the database is lost too. If the runs show it, rewrite
  the export at startup when it differs from the table.
- **Does `synchronous=NORMAL` lose a curation change?** C2 answers it.
  If a change can be lost while the screen already showed it, curation
  commits may need `synchronous=FULL` for that one statement.
- **What the clock does after a sysrq reboot.** A real pull boots at
  2021-05-15 02:00. Check that a sysrq reboot does the same, or C6 has
  to use a real pull.
- **No network at boot.** If NTP never comes, the 600 s grace runs out
  at a false 02:10 and the wake alarm is set for a false 05:00. Testing
  it needs Wi-Fi off, which needs USB adb (wireless adb is the only way
  in otherwise). Left out unless the maintainer wants it.
