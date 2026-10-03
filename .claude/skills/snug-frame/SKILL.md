---
name: snug-frame
description: Operate the SNUG Frameo photo frame (SNFRM-8GB10-BK, RK3126, Android 6) that Raam runs on, over adb and root. Covers finding and connecting to it, putting a Raam build on it and proving the new build runs, uninstalling it, logs, screenshots, scripted taps, the debug.video.* test props, the sleep/wake and fault-injection checks, raam.db edits, memory readings, Frameo and the home app, and the recovery ladder down to Rockchip flash mode. Use it whenever a task touches the real frame in raam, even when it's only a step on the way: "on the frame", "on the device", "the SNUG", adb, logcat, screencap, install, update or uninstall Raam, "is it running", "the frame is black / unreachable", Wi-Fi or wireless adb on the frame, Frameo, flash mode, partitions, or a fresh-frame setup.
---

# Operating the SNUG frame

The frame is a 10.1-inch SNUG Frameo (SNFRM-8GB10-BK): Rockchip RK3126,
Android 6.0.1 (API 23), 1280×800 landscape (the panel is portrait,
rotated by `ro.sf.hwrotation=270`), about 493 MB of RAM, no Google Play
Services. `su` is baked into the firmware, so `adb shell su -c '…'` is
root even though `adb root` is refused. Raam is the app being built
for it; Frameo is the vendor app it replaces.

Two things shape everything below:

- **It is a real, single, shared device.** One person uses it as their
  photo frame, other Claude sessions may be working on it, and a
  mistake below Android can brick it. Read before you write.
- **This repo is public.** Never put the frame's serial, its IP
  address, MACs, SSIDs or the backups' path into a file in this repo,
  a commit or a PR. They live in the untracked `.env`, in local memory
  and in the brain.

## Find the frame

`scripts/frame` in this skill finds it, runs the common checks, and
does every procedure below as one command that checks what it needs
first and proves its result (`$S help` lists them). Use the command,
not the steps by hand: the steps are here so you know what it does and
what its FAIL means. Run it from the repo root:

```sh
S=.claude/skills/snug-frame/scripts/frame
$S status      # transports, uptime, screen, Wi-Fi, Raam/Frameo state, props set
T=$($S target) # the adb -s value; the commands below use $T
```

Each Bash call is a fresh shell, so set `S` and `T` again in each one
(or prefix commands with them).

How it picks: `RAAM_FRAME_SERIAL` from `.env` is the USB serial. Over
Wi-Fi the frame shows up as `<ip>:5555` instead, and both can be
connected at once, so **always pass `-s`**. The script prefers USB,
then any `product:SNFRM` device. If neither is there, the address is
in local memory (`frame-recovery-paths`) or the brain (search "frame
IP wireless adb"); `adb connect <ip>:5555`. The DHCP lease drifts, so a
dead address may only mean it moved: look up the frame's address on
the router (ask the maintainer) before assuming Wi-Fi is down.

Start any session that will touch the frame with `$S status`. The
frame's state changes between sessions (factory resets, restores,
Frameo self-updates), so don't trust a remembered state: check what's
installed, what's home, which debug props are set.

## How careful to be

Sort what you're about to do into one of four tiers, and act
accordingly.

1. **Look.** `getprop`, `dumpsys`, `logcat -d`, `screencap`, `ps`,
   reading files as root, pulling files. Go ahead.
2. **Raam-level, undone by a restart.** `install -r` of Raam,
   `am start`/`force-stop`, `input tap`/`keyevent`, `setprop
   debug.video.*`, `pm grant` to Raam. Go ahead in a dev session, and
   put things back when done: props cleared, Raam in front.
3. **Persists on the frame.** `pm disable-user`/`enable`, `persist.*`
   props, `settings put`, Wi-Fi off, reboot, editing `raam.db`,
   uninstalling. Say what you're about to do and why first. If it's a
   setup or debug aid that stays on the frame, record it in
   `docs/FRAME-SETUP.md` in the same change (what, why, undo). That's
   the maintainer's standing rule: a fresh frame must be rebuildable from
   that file.
4. **Below Android.** Writing `/system`, `dd` onto a block device,
   anything with `rkdeveloptool wl`, a factory reset, `reboot recovery`.
   Needs the maintainer's explicit yes for this specific action, a fresh
   verified backup, and a way back. Claude Code's auto-mode classifier
   blocks these even after a yes, so write the steps as a script (with
   `set -e`, checks and a read-back) and have the maintainer run it as
   `! sh <script>`. **Never write `uboot`**: it holds the flash mode
   that every other recovery depends on.

Turning Wi-Fi off, or anything that might, is tier 3 with a trap: over
wireless adb you lose the frame. Do it only with USB connected.

**Another session may hold the frame.** Before tier 2 or above, check
`ListAgents` for another session in raam or immich-frame-rs and ask it
with `SendMessage`. Large transfers over Wi-Fi and USB re-plugs disturb
timing-sensitive work in another session.

## Raam on the frame

Package `io.github.noctonca.raam`; activity
`android.app.NativeActivity`; log tag `raam`; database
`/data/user/0/io.github.noctonca.raam/files/raam.db`; photos folder
`/sdcard/Pictures/Frame`.

### Build, install and prove it's the new build

```sh
$S build                 # env check, then a release APK of this checkout
$S deploy                # build, install -r, restart, prove the new pid
$S install [DIR]         # fresh install; DIR restores an app-data backup
$S backup DIR            # raam.db, photos and export into a new folder
$S uninstall DIR|--no-backup
```

Each prints `ok` lines for what it proved, the new pid's first log
lines among them (`db: loaded N saved settings`, the startup sweep,
`Immich now online`), and stops at the first `FAIL`. `build` needs
`.env` (the versioned `ANDROID_NDK_ROOT` and both
`CARGO_APK_RELEASE_KEYSTORE` variables); its log goes to
`target/frame-build.log`. `deploy` and `install` always build first,
so an APK left in `target/` from an older checkout never reaches the
frame.

What the commands take care of, and why:

- **`install -r` doesn't kill the running process here.** The old pid
  can keep logging for minutes, and `am start` only brings the old task
  to the front. `deploy` force-stops and starts, then waits for a
  process other than the old one to log `EGL + pipeline + painter
  ready`. When Raam is home the system relaunches it within seconds of
  a force-stop, so the pid can change twice; the last one is the one
  proved. To read the log yourself: `$S pid`, then `$S log`. The ring
  holds only a few minutes.
- **A fresh install grants storage before the first launch**, so the
  curation import (which reads `/sdcard` and runs only while the
  curation table is empty) lands on boot one.
- **`install DIR` restores before the first launch.** Raam missing from
  a frame where it used to be usually means a factory reset or a
  restore (the brain's latest hand-off normally says which). If the
  maintainer wants their old Raam back, pass the backups folder's newest
  `app-data-*`. The command checks it first (`raam.db` checkpointed and
  passing `integrity_check`), pushes `Frame/` to `/sdcard/Pictures/Frame`
  and `frame-curation.json` beside it, installs, grants, copies the
  database in owned by the app's uid (read from the frame: it changes
  with every install), launches, and fails unless Raam loaded the
  restored settings. On the first launch the startup sweep drops every
  cache row whose file isn't there (all of them, after a restore), and
  Raam fetches those photos again. Without DIR, Raam starts empty and
  the maintainer re-enters the server and albums.
- **`backup DIR` stops Raam and copies in one root script**, because a
  home Raam is relaunched about 2 s after a stop. It checkpoints the
  copy locally, so the folder holds `raam.db` alone (the shape `install
  DIR` expects), and refuses a folder that exists. Keep backups in the
  backups folder (local memory has its path), never in the repo.

### Uninstall

`$S uninstall DIR` is tier 3: say what you're about to do and why
first. An update never needs it (`deploy` keeps the data). Uninstalling
deletes Raam's data folder, `raam.db` with it (settings, server key,
albums, curation), so the command backs up into DIR first; pass
`--no-backup` only on the maintainer's word that the data can go. It then
clears the props, gives the home back to Frameo if it's disabled (a
plain uninstall while Raam is home leaves the frame with no home app),
uninstalls, reboots, and proves Raam is gone and Frameo is in front.

It keeps `/sdcard/Pictures/Frame`, the curation export and the
development changes in FRAME-SETUP §3 (wireless adb, the disabled
updaters, the boot hook): they're the way back in. Delete the photos
folder and the export only on the maintainer's word.

### Seeing and touching it

- **Screenshot:** `$S shot out.png`, or `adb -s $T exec-out screencap
  -p > out.png`. Each takes 4–5 s on this device, too slow for two
  shots inside one 10 s dwell.
- **Motion** (transitions, Ken Burns, video): `adb -s $T shell
  screenrecord --time-limit 8 --size 1280x800 /data/local/tmp/r.mp4`,
  pull it, then extract frames with `ffmpeg -fps_mode vfr` (not
  `-vsync`).
- **Live view and control:** `scrcpy -s $T` (installed on the Mac).
- **Taps:** `adb -s $T shell input tap X Y`, in screen coordinates
  (1280×800). A tap on the slideshow opens the menu (640 400 is safe).
  The menu closes itself 15 s after the last input, and a tap after
  that re-opens it instead of pressing a button. Check logcat's
  "overlay opened by tap" / "overlay closed" to see which happened. A
  synthetic tap's down and up arrive together, so egui can register
  the click one frame late: wait a second and re-read before deciding
  it missed. The menu row is centred, so button positions shift when
  the set of buttons changes. Screenshot first, then aim.
- **Raw touch** (`getevent`, `/dev/input/event0`) is not in screen
  coordinates: screen x = (800 − raw y) × 1.6, screen y = raw x ×
  0.625. MotionEvents and `input tap` are already mapped.
- **Keys:** `input keyevent 224` wakes the screen, `223` sleeps it.
- **Another app in front:** there's no `cmd` binary on Android 6.
  `am start -n pkg/activity` works, or `monkey -p <pkg> -c
  android.intent.category.LAUNCHER 1`. Bringing Frameo to the front
  can get Raam killed for memory.

### Test props

`adb -s $T shell setprop debug.video.<name> <value>`; `setprop <name>
""` clears one, and a reboot clears them all. Raam reads them once per
loop pass, so most take effect live. `$S props` lists what's set, and
`$S clear-props` clears every one.

| Prop | Values | Effect |
|---|---|---|
| `fail` | `rt` `probe` `live` `hang` `open` `panic` | fault injection (see the reference) |
| `sleep`, `wake` | `HH:MM` | override the sleep schedule. Equal times mean never asleep |
| `idle` | seconds | how long a hand-woken screen stays on in sleep hours |
| `mech` | `flags` `wakelock` `both` | how the screen is turned on |
| `only_videos` | `1` | queue only clips |
| `only_path` | text | queue only items whose path contains it |
| `hold_first` | `1` | hold a live clip on frame 0 |
| `show_still` | `1` | draw the composed still instead of the live clip |
| `audio_extra_ms` | ms | override the audio delay calibration |

Clear every prop you set before you finish. A forgotten `sleep`/`wake`
override silently replaces the maintainer's schedule until the next reboot.

### Deeper checks

[references/raam-checks.md](references/raam-checks.md) has the proven
recipes and the log lines that mean success:

- the compressed sleep/wake window (the whole schedule in about 5 min);
- fault injection and recovery (`fail=rt`), required after risky
  pipeline changes;
- the `raam.db` edit cycle, and its chown trap;
- sound-on clips;
- reading memory honestly (PSS alone understates it badly);
- reading a sleep or wake the log has already lost.

## Frameo, the home app and setup

[docs/FRAME-SETUP.md](../../../docs/FRAME-SETUP.md) is the source of
truth for every change a frame gets: ADB, installing, permissions,
disabling Frameo, wireless adb, updaters, the boot hook, and each
one's undo. Follow it, rather than a remembered command. The pieces
that bite:

- **The ten seconds.** After `pm disable-user`/`enable`, package state
  reaches disk about ten seconds later (12 s measured), and a sooner
  reboot loses it. `$S frameo` waits for it in
  `/data/system/users/0/package-restrictions.xml` (`enabled="3"` is
  disabled-user), then syncs, then reboots. Do the same by hand for any
  other package.
- **A reboot is proved by a new `boot_id`.** Right after `adb reboot`
  over Wi-Fi, the old boot can still answer `sys.boot_completed`.
  `$S reboot` (and every command that reboots) waits for
  `/proc/sys/kernel/random/boot_id` to change. The first seconds after
  a boot log DNS failures and a 2021 clock: the frame has no RTC, and
  Wi-Fi and NTP aren't up yet. Raam's album sync and weather recover on
  their own within about a minute and a half.
- **Frameo keeps its own schedule while it's enabled.** It arms a 23:00
  standby alarm (`dumpsys alarm`, `StandbyBroadcastReceiver`). If Raam
  runs over an enabled Frameo, the two schedules fight. Disabling Frameo
  removes the alarm.
- **ADB on the SNUG** is on in the firmware (`persist.sys.usb.config=adb`).
  A factory-fresh Frameo 1.13.4 leaves USB adb up. An updated Frameo
  (1.32.x) needs "Transfer from computer" on. The beta-program route
  is untested.
- **Wireless adb** is `su -c 'setprop persist.adb.tcp.port 5555'` plus a
  reboot. It survives reboots, and a factory reset removes it.
- **Frameo self-updates** over Wi-Fi from the factory 1.13.4 in
  `/system` to 1.32.x in `/data/app`. The vendor updaters
  (`com.adups.fota`, `com.adups.fota.sysoper`,
  `android.rockchip.update.service`) must be disabled again after any
  reset, before Wi-Fi.
- Frameo's own state that Raam relies on (SystemUI disabled, no screen
  timeout, fixed brightness, no lock screen) is listed in FRAME-SETUP
  §2.

### Disabling Frameo, not uninstalling it

Disabling Frameo makes Raam the only home app. It's tier 3. Do it only
with Raam installed: a frame with Frameo disabled and no Raam has no
home app. Set Wi-Fi and brightness in Frameo first, because Raam has no
settings for them on Android yet (FRAME-SETUP §2).

```sh
$S frameo disable   # refuses without Raam installed; proves Raam home, no Frameo alarms
$S frameo enable    # the undo
```

**Frameo is disabled, never uninstalled.** The factory Frameo
(1.13.4) lives in `/system/priv-app`, so removing it means writing
`/system`, which is tier 4. Disabling gets the same result and can be
undone. Don't go further unless the maintainer asks for it.

**Undoing Frameo's self-update** (untested on this frame): `pm
uninstall net.frameo.frame` should remove only the 1.32.x update in
`/data/app` and leave the factory 1.13.4. That is still tier 3:

- Whether Frameo's data (friends, photos list) survives is unknown,
  so back it up first.
- Whether the disabled state survives is also unknown. Re-check
  `package-restrictions.xml` afterwards.
- An enabled Frameo on Wi-Fi updates itself again.

If you try it, record the outcome in this skill and in
`docs/FRAME-SETUP.md`.

## When it's unreachable or won't boot

[references/recovery.md](references/recovery.md) has the ladder, from
lightest to deepest: wireless adb, a power cycle, USB adb, Rockchip
flash mode. It also covers the cable and hot-plug traps, the flash-mode
address shift, and what not to restore. Read it before doing anything
beyond tier 2 to get back in. Most "the frame is dead" moments so far
were Wi-Fi being down or the address having moved.

The job here is getting back in and saying what's wrong. Once adb
answers, report what you found and answer the question you were
asked: why it was unreachable, and why the screen looks the way it
does. Other things you notice, like Raam missing, the boot hook gone
or a stale APK, go in one short "also noticed" list as offers. Don't
fold reinstalling or reconfiguring into the recovery plan: the maintainer
asked for the frame back, not a rebuild.

## After a session on the frame

- Leave it as you found it: props cleared, Raam (or whatever was) in
  front, recorders stopped, `/sdcard` and `/data/local/tmp` scratch
  removed.
- Anything that persists goes in `docs/FRAME-SETUP.md`.
- Capture durable findings in the brain (a new trap, a changed address,
  the frame's state at the end), never in the repo if they're
  device-identifying.
