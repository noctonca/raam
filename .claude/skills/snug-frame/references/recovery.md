# Getting back into the frame

From the lightest way in to the deepest. Stop at the first rung that
works. Most "the frame is dead" moments so far were Wi-Fi being down or
the frame's address having moved, not a broken frame.

The device-specific details (the address, the backups' folder, the
partition dumps and their md5s) are private. They're in local memory
(`frame-recovery-paths`) and the brain (search "frame recovery flash
mode backups"). The backups folder's `README.md` has the full
flash-mode procedure and the LBA table. Read it before any write.

## 0. Is it really gone?

- `adb devices -l`: the frame may be there under the other transport
  (USB serial vs `<ip>:5555`).
- The address may have moved (DHCP). Ask the owner for the frame's
  address on the router, then `adb connect <ip>:5555`.
- A dark screen alone means little: Raam may be in its sleep hours, or
  Frameo's sleep mode is on. `input keyevent 224` wakes it if adb
  answers.
- Ask the owner what they see on the screen. They're next to it.

## 1. Wireless adb

`adb connect <ip>:5555`. It works while Android runs, Wi-Fi is on and
`persist.adb.tcp.port` is 5555. A factory reset clears that prop:
re-set it over USB (FRAME-SETUP §3).

## 2. A power cycle

This undoes "Wi-Fi off", **but only if the boot hook is installed**:
`/system/bin/raam-boot.sh`, called from the end of
`/system/bin/install-recovery.sh` (FRAME-SETUP §4). It turns Wi-Fi on at
every boot. Check before you rely on it: `adb shell su -c 'tail -3
/system/bin/install-recovery.sh'`. A restore of an older `/system`
removes it. After a boot, `adb logcat -d -s raam-boot` says what it did.

## 3. USB adb

The cable matters, and so does the order:

- Use the old micro-B to USB-A cable, with an A-to-C adapter, into the
  Mac's left port. The newer cable charges only: the frame reports
  `vbus_status=2` and never appears in `adb devices`.
- **Plugging the cable into a running frame resets it.** Its USB power
  line is tied to its own supply, and the Mac's port browns it out. Plug
  in once, at the start of a session, and leave it in. Tell the owner
  before asking them to plug in, because the frame will restart.
- USB adb stays up through boot, Frameo's first-run setup and an
  updated Frameo's start (checked with Frameo 1.13.4 and 1.32.12).

Prefer USB for big transfers (partition dumps, many photos): Wi-Fi
gives about 2–3 MB/s at best, and drops to well under 1 MB/s when
another session is using the frame.

## 4. Rockchip flash mode

For when Android doesn't boot. The bootloader in `uboot` has a USB flash
mode ("Loader") that works without Android. Tested on this frame:
reads, and writes (a pattern in the unused `radical_update` partition,
and a 7.5 GB restore, both read back byte-identical).

```sh
adb -s <usb-serial> reboot bootloader   # the screen stays dark
R=~/code/tools/rkdeveloptool/rkdeveloptool
$R ld                     # DevNo=1 Vid=0x2207,Pid=0x310d … Loader
$R rl <lba> <sectors> out.img
$R wl <lba> image.img     # tier 4: the owner runs it, from a script
$R rd                     # reboot into Android; pulling power also works
```

If Android is too broken for `adb reboot bootloader`, ask the owner;
the button combination for this frame is untested.

- **Addresses are shifted.** Flash-mode LBA = Android sector − 8192
  (0x2000). The parameter block at flash LBA 0 holds the partition map
  (`mtdparts`), and its offsets are the flash-mode LBAs. `rkdeveloptool
  ppt` says "Not found any partition table!" because there's no GPT,
  and that's expected.
- **Partition map** (flash LBA, sectors): uboot 0x2000 0x2000 · misc
  0x4000 0x2000 · resource 0x6000 0x8000 · kernel 0xE000 0x6000 · boot
  0x14000 0x6000 · recovery 0x1A000 0x10000 · backup 0x2A000 0x20000 ·
  cache 0x4A000 0x40000 · metadata 0x8A000 0x8000 · kpanic 0x92000
  0x2000 · system 0x94000 0x200000 · radical_update 0x294000 0x20000 ·
  userdata 0x2B4000 to the end.
- **Never write `uboot`.** It holds this mode. Below it is the chip's
  MaskROM mode, which needs a pin shorted on the board and is untested.
- Before any write, read the target range back and compare md5s with
  the backup, so you know the map is right. After the write, read it
  back again.

## Restoring

- Write restores as a script with `set -e`, checks on each image's size
  and md5 against the backup's record, the write, and a read-back
  compared with the md5. The backups folder's `scripts/` holds the ones
  used before (`restore-0923.sh`, `write-test.sh`, `factory-reset.sh`).
  Copy their shape. The owner runs it with `! sh <script>`.
- **A dump of a live, mounted partition can be torn.** The September
  `userdata` image was taken while Android ran, and restoring it
  boot-looped Android (PackageManagerService choked on `packages.xml`).
  Before relying on a live dump of `userdata`, `cache` or `metadata`,
  check a copy with `e2fsck -n` and `debugfs`. `system`, `boot` and the
  rest are effectively static and safe.
- A single-pass `dd … | tee img | md5` is the only meaningful check for
  a live partition: hashing on the frame and again after the pull
  differs just because minutes passed.
- Dumping partitions, read-only: `adb exec-out su -c 'dd
  if=/dev/block/mmcblk0pN bs=4096' > name.img`, over USB, streamed
  with no copy on the frame. The first 4 MB (the parameter block) is in
  no named partition: `rkdeveloptool rl 0 8192 parameter-region.img`.
  Keep dumps off the frame and out of this repo.

## A factory reset

The lightest full reset, through the stock recovery. It wipes `userdata`
and `cache` and boots Frameo 1.13.4's first-run setup:

```sh
adb shell "su -c 'mkdir -p /cache/recovery && echo --wipe_data > /cache/recovery/command && sync'"
adb reboot recovery
```

This is tier 4: back up the app data first (`raam.db` checkpointed,
the curation export, `/sdcard/Pictures/Frame`). Afterwards, USB adb is
up but nothing else: wireless adb, disabled updaters, Raam and Frameo's
"disabled" state are all gone. Disable the updaters before the frame
joins Wi-Fi (Frameo self-updates), then work through FRAME-SETUP.
The owner does Frameo's onboarding by touch. Frameo's licence files
come back on their own.

## Known traps

- SuperSU's `/system/su.d` scripts never run on this ROM (`supolicy`
  never runs). The only root boot hook is `install-recovery.sh`.
- `install-recovery.sh` logs `Installing new recovery image: failed` at
  every boot. That's the stock script, and harmless: the recovery
  partition stays byte-identical.
- No emulator on the Mac runs the frame's 32-bit ARM image. There's no
  rehearsal for a risky write except the real frame.
