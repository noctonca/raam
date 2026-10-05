# Raam checks on the frame

Proven recipes, each with the log lines that mean it worked. `$T` is
the `adb -s` target (`scripts/frame target`). Log lines are matched by
their text. The module prefix moved from `raam_android` to
`raam_core::app` for controller lines, so grep on the message, not the
prefix.

## Contents

- [Sleep and wake in five minutes](#sleep-and-wake-in-five-minutes)
- [A sleep or wake the log has lost](#a-sleep-or-wake-the-log-has-lost)
- [Fault injection](#fault-injection)
- [Editing raam.db](#editing-raamdb)
- [Power cuts](#power-cuts)
- [A clip with sound](#a-clip-with-sound)
- [Memory](#memory)
- [Baselines](#baselines)

## Sleep and wake in five minutes

This checks the whole schedule state machine without waiting for
night.

1. Read the frame's clock: `adb -s $T shell date +%H:%M:%S`. Don't use
   the Mac's.
2. Set sleep about 2 min ahead, wake 4 min after that, and a short
   idle:
   ```sh
   adb -s $T shell setprop debug.video.sleep 14:32
   adb -s $T shell setprop debug.video.wake 14:36
   adb -s $T shell setprop debug.video.idle 20
   ```
3. Watch: `adb -s $T logcat -v time -s 'raam:*' | grep -i -e sleep -e wake -e idle -e visible`.

Expected, in order:

- at the sleep minute, within about 30 ms: `sleeping at HH:MM (sleep
  time), wake alarm set for HH:MM (in Ns)`. The screen goes off 4–5 s
  later (root keyevent 223).
- `adb -s $T shell input keyevent 224` wakes it by hand: `visible
  again`, then `woken by hand in sleep hours, back to sleep after 20s
  untouched`.
- exactly `idle` seconds later: `idle in sleep hours`, with the alarm
  re-armed for the same wake time.
- at the wake minute, within about 100 ms: `woken at wake time`.

Clear all three props afterwards. With them cleared, the saved schedule
applies again. To keep the frame awake while testing something else
during sleep hours, set `sleep` and `wake` to the same time: equal
times mean never asleep.

## A sleep or wake the log has lost

logcat holds minutes, so an overnight wake is gone by morning. `adb -s
$T shell dumpsys power | grep -e mLastWakeTime -e mLastSleepTime -e
mWakefulness` gives "(N ms ago)". Subtract N from the frame's `date`.
`ps` start time on Raam's pid shows whether the process survived the
night (it should: sleep and wake need no restart).

## Fault injection

Run this after any change to the GPU, decoder or pipeline recovery
paths. AGENTS.md forbids weakening them without re-running it.

| `debug.video.fail` | What fails |
|---|---|
| `rt` | every render target made after startup (gl.rs) |
| `probe` | the probe decoder, at once |
| `live` | the live decoder, at once |
| `hang` | the live decoder, which then holds its release 15 s like a wedged stop |
| `open` | opening a player, like a failed SurfaceTexture setup |
| `panic` | panics right after startup (needs a restart to take effect) |

All but `panic` act live. Under `rt`, expect `no transition scratch …
cutting instead` and `plan N dropped: … next plan in 5s`, and Raam
stays up. A clip already playing plays through. Clear it with
`setprop debug.video.fail ""`: plans compose again and transitions
come back. Seeing recovery after the clear is the point of the test.
Then check what the run left behind: Settings → Videos should still
show every clip ready to play. A decoder fault backs clips off but
never marks them unplayable (#121); before that fix, `probe` marked
every clip it touched.

## Editing raam.db

Raam keeps settings and curation in SQLite (WAL). To change a setting
it has no UI for, or to set up a test, copy the database off the
frame, edit it, and copy it back, with Raam stopped. This is tier 3
(it persists), so say so first.

```sh
P=io.github.noctonca.raam; D=/data/user/0/$P/files
adb -s $T shell "su -c 'cp $D/raam.db $D/raam.db-wal $D/raam.db-shm /sdcard/; chmod 666 /sdcard/raam.db*'"
for f in raam.db raam.db-wal raam.db-shm; do adb -s $T pull /sdcard/$f .; done
sqlite3 raam.db 'PRAGMA wal_checkpoint(TRUNCATE);'
# check it's current (the playback/queue state is minutes old, not days), then edit
sqlite3 raam.db "…"
adb -s $T push raam.db /sdcard/raam.db
adb -s $T shell "su -c 'am force-stop $P; cp /sdcard/raam.db $D/raam.db; rm -f $D/raam.db-wal $D/raam.db-shm; sleep 1; am force-stop $P'"
adb -s $T shell rm -f /sdcard/raam.db /sdcard/raam.db-wal /sdcard/raam.db-shm
```

- **Don't chown when editing.** `cp` onto the existing file keeps the
  app's ownership. A guessed uid once made the database unreadable to
  Raam. If it happens, `ls -n` the files dir for the real uid and
  `chown <uid>:<uid>`.
- **Restoring onto a fresh install** is `frame install DIR` (SKILL.md):
  there's no existing file to copy over, so the database has to go in
  owned by the app's uid, before the first launch.
- **The double force-stop is required.** When Raam is home, the system
  relaunches it about 2 s after the first stop, and that process opens
  the database mid-copy. The second stop kills it. Expect the pid to
  change twice.
- **That second stop can wedge the video decoder.** If the relaunched
  process had already opened a clip, killing it mid-decode leaves
  mediaserver's decoder stuck (`OMXNodeInstance … failed to enter Idle
  state … aborting` in the full logcat). Raam then logs `no first frame
  within 10s` and `a stopped decoder not released after 10s`, and no
  clip plays again until a reboot. Seen 2026-10-04. With clips in the
  queue, finish the cycle with `$S reboot` (it also clears test props).
- **It can wedge audio too, and then Raam can't start.** After a
  `deploy`'s stops, the new process can log `overlay ready` and the
  transitions linked, then nothing: no `pipeline + painter ready`, and
  `deploy` FAILs after 90 s. `su -c 'debuggerd -b <pid>'` shows the
  render thread (the first `android_main` thread) in a binder call under
  `AudioSystem::getOutputLatency`, with mediaserver in `futex_wait`. It
  isn't the build: `$S reboot`, and the same build comes up. Seen
  2026-10-05.
- Settings are rows in the `setting` table, values in JSON. A default
  is an absent row, so restore a default by deleting the row. Example:
  `video.sound` = `true` turns clip sound on.
- Keep a copy of the pulled original until you've seen Raam start
  cleanly on the edited one.

## Power cuts

`scripts/pulltest` aims a power cut at a write window and checks what
survived ([docs/plan/power-cut-tests.md](../../../../docs/plan/power-cut-tests.md)
has the cases). It reboots the frame on every run: tier 3, so say so
first. Runs and their files go to `target/pulltest/`, one folder each,
and every result row is appended to `target/pulltest/results.tsv`.

```sh
P=.claude/skills/snug-frame/scripts/pulltest
$P setup                                   # 8 test photos, only_path, restart
$P arm C2 'Fill/Fit override' --delay 50   # cut 50 ms after the commit's line
$P curate fit                              # menu, then Fit to frame
$P check                                   # new boot, copy, check, result row
# a preview's rename, held 30 s, cut 1 s into the hold:
adb -s $T shell setprop debug.video.stall rename@prefetch:30
$P arm C3 'debug.video.stall: rename' --delay 1000
$P evict 1
$P check
$P teardown                                # always, at the end
```

- **Every reboot clears the props**, `only_path` and `stall` with them.
  After a check, set `only_path=pulltest` again (or run `setup` again)
  before the next curation run.
- **The cut is `echo b > /proc/sysrq-trigger`**, fired by a watcher on
  the frame that follows `logcat`. It runs inside an adb session the
  Mac holds open: on this frame nothing started in the background
  outlives its adb session, not even with `nohup` or `setsid`. If the
  Mac lets go, the watcher dies with it, and it gives up by itself after
  `--for` seconds (300 by default). `disarm` stops it.
- **`--real`** arms no watcher: the run is recorded for a pull by hand.
  Hold the window with a 60 s stall, check its marker is in the run's
  `before.log`, then tell the person at the frame to pull, and to plug
  back in after about 5 s. Proven on 2026-10-03 with C1, C3 and C4: a
  chat message is fast enough inside 60 s. The Mac also says "pull now",
  but nobody at the frame may hear it. A hand can't time C2's
  milliseconds; C1 covers the same commit.
- **`check --grace`** follows the log for 11 minutes before copying
  anything, because stopping Raam would restart the sleep grace timer.
- **The row:** `integrity` and `rows` (each `cached_file` row against
  its file's size) must be `ok` and all matching, `strays` (files with no
  row, or empty) 0, and the new Raam `ready`. `change` is the aimed
  curation change, `kept` or `lost` in the database; `export` is whether
  the export matches the database; `asset` is the stalled preview's
  fate. `boot_clock` is the first log line's time: a cut boots in May
  2021 until NTP.
- **`teardown`** removes the test photos, clears the props, and deletes
  the test photos' curation rows from `raam.db` (the edit cycle above,
  for those rows only). The export keeps them until the next curation
  change rewrites it.
- **The taps** are measured for the menu over local photos. The tap
  that opens the menu also picks the middle tile of a collage, and the
  change goes to that tile. `PULLTEST_TAP_FIT` and `PULLTEST_TAP_HIDE`
  override the positions if the menu changes.

## A clip with sound

Daytime only: it's a household frame. Set `debug.video.only_videos 1`
so clips come at once, then set `video.sound` to `true` with the
raam.db cycle. Success: `audio delay applied: … ms`, `clip N starting
(Continue, sound on)`, `OpenSL ES player ready`, `clip playing, …
(audio pre-rolled)`, `sound started (position … ms, … ms output
latency)`. A "max A/V drift ~300 ms" at the end of a clip is the
picture clock rebasing when audio latches, not drift during play.
Speaker volume: `dumpsys audio`, STREAM_MUSIC, the speaker's Current
value. Delete the row and clear the prop afterwards.

## Memory

PSS badly understates memory here: GPU and buffer memory sit outside
it ("Lost RAM" in `dumpsys meminfo`). An app's real footprint is the
sum of three:

1. PSS: `adb -s $T shell dumpsys meminfo io.github.noctonca.raam`, the
   TOTAL row.
2. Mali: `su -c cat /sys/kernel/debug/mali/gpu_memory`, the row for
   Raam's process (`android_main`). `max_mali_mem` is a lifetime peak:
   ignore it for steady state.
3. ion: `su -c 'cat /sys/kernel/debug/ion/heaps/*'`, Raam's client row
   (decoder buffers; it rises a lot while a clip plays).

Measure at a quiet steady state: some time after sync, prefetch or a
UI session, or the allocator's free pages inflate PSS by tens of MB.
Baselines: quiet stills about 56–58 MB in total (PSS ~17 + Mali ~23–26
+ ion ~16). The budget line is 80 MB. A playing clip pushes ion to
~100 MB. `MemFree` floats between ~7 and 75 MB, which is normal on this
frame.

## Baselines

Use these to judge what you see. Taken from logcat in September 2026,
and still current unless the pipeline changed:

- dwell at "every 10 s": 10.0 s from transition end to the next start;
- transitions at the 1.3 s setting: 1.33–1.45 s;
- a clip starts 0.6–0.8 s after its slide lands (probe frame 0 at
  ~370–510 ms), with 0–1 dropped frames;
- first collage about 1.5 s after launch, from the saved queue;
- idle about 53 fps, menu open 49–52 fps.
