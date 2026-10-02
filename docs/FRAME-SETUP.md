# Setting up a frame

Every change made on a frame, for Raam or for working on it: what it
is, why, and how to undo it. Follow it top to bottom on a fresh frame;
add to it in the same change as anything new put on a frame.

The README's [Putting it on a frame](../README.md#putting-it-on-a-frame)
is the user's path. This file repeats those steps for completeness,
then adds what a development frame gets on top. Commands run from a
computer with the frame on USB; `su` is the frame's own root (see the
README's checks).

## 1. What Raam needs

| Change | Command | Why | Undo |
|---|---|---|---|
| ADB on | in Frameo, turn on Transfer from PC | the only way in. The SNUG's firmware has ADB on (`persist.service.adb.enable=1`, `persist.sys.usb.config=adb` in `/system/build.prop`) but Frameo keeps the USB port off; Transfer from PC opens it in photo mode (`svc usb setFunction ptp`) and ADB comes with it. Frameo's documented route, Settings → About → Beta program → ADB access, is untested here | the same switch |
| Raam installed | `adb install raam-android.apk` | — | `adb uninstall io.github.noctonca.raam` |
| Storage permission | `adb shell su -c 'pm grant io.github.noctonca.raam android.permission.READ_EXTERNAL_STORAGE; pm grant io.github.noctonca.raam android.permission.WRITE_EXTERNAL_STORAGE'` | the photos folder and the curation import. Raam grants itself this through `su` at start, but granting before the first launch lets the import land on boot one | `pm revoke` the same two |
| Frameo disabled | `adb shell su -c 'pm disable-user --user 0 net.frameo.frame'`, wait ten seconds, `adb reboot` | Raam becomes the only home app | `su -c 'pm enable net.frameo.frame'`, wait ten seconds, reboot |

The ten seconds: the package state is written to
`/data/system/users/0/package-restrictions.xml` about ten seconds after
the change, and a sooner reboot loses it.

## 2. Left behind by Frameo

Set Wi-Fi and brightness in Frameo before disabling it: Raam has no
settings for them on Android yet. Disabling Frameo also keeps the
state it set, which Raam relies on:

| State | Value seen | Effect |
|---|---|---|
| `com.android.systemui` | disabled | no status bar, navigation bar or lock screen |
| `system screen_off_timeout` | 2147483647 | the screen never times out; Raam's sleep schedule turns it off |
| `system screen_brightness_mode`, `screen_brightness` | 0 (manual), 204 | fixed brightness |
| `secure lockscreen.disabled` | 1 | no lock screen |

A frame that never ran Frameo would need these set by hand
(`adb shell settings put …`, `su -c 'pm disable-user --user 0 com.android.systemui'`).

## 3. A development frame

| Change | Command | Why | Undo |
|---|---|---|---|
| Wireless ADB | `adb shell su -c 'setprop persist.adb.tcp.port 5555'`, then `adb reboot`; connect with `adb connect <frame-ip>:5555` | work without the cable. A persistent property: it survives reboots | `su -c 'setprop persist.adb.tcp.port ""'`, reboot |
| Updaters disabled | `adb shell su -c 'pm disable-user --user 0 <pkg>'` for `com.adups.fota`, `com.adups.fota.sysoper`, `android.rockchip.update.service` | a vendor update must not replace the system under Raam | `pm enable <pkg>` each |
| Debug switches | `adb shell setprop debug.video.<name> <value>` | test paths ([ARCHITECTURE.md](ARCHITECTURE.md)); not persistent | `setprop debug.video.<name> ""` or reboot |

Reserving the frame's address on the router keeps `adb connect`
working across DHCP leases.

## 4. Recovery

From the lightest way back in to the deepest:

1. **Wireless ADB**, while Wi-Fi works.
2. **A power cycle**, if Wi-Fi was turned off: the boot hook below
   turns it back on.
3. **USB ADB**, while Android boots. Keep the cable in for a whole
   session: plugging a computer into a running SNUG resets it (its USB
   power line is tied to its own supply). A cable that charges but
   never shows up in `adb devices` doesn't carry data.
4. **Rockchip flash mode**, when Android doesn't boot. `adb reboot
   bootloader` (the screen stays dark), then
   [`rkdeveloptool`](https://github.com/rockchip-linux/rkdeveloptool):
   `rkdeveloptool ld` lists the frame as `Loader`, `rl`/`wl` read and
   write by sector, `rd` reboots. On the SNUG (RK3126) flash-mode
   sector numbers are Android's minus 8192 (0x2000); the parameter
   block in the first 4 MB holds the map (`mtdparts`), which
   `rkdeveloptool ppt` doesn't read. Never write `uboot`: it holds
   this mode. Below it is the chip's MaskROM mode, which needs a pin
   on the board shorted (untested).

Before changing anything below Raam, dump every partition (`su -c 'dd
if=/dev/block/mmcblk0pN'` through `adb exec-out`, one pass with a hash)
and the first 4 MB through flash mode (`rkdeveloptool rl 0 8192`),
which no named partition covers. Keep the dumps off the frame.

### Wi-Fi back on at every boot

So that a power cycle undoes "Wi-Fi off" without ADB or a screen.
Installed on the first frame; checked by turning Wi-Fi off and
rebooting (`adb logcat -d -s raam-boot` says what it did). The
only root boot hook on the SNUG is init's `flash_recovery` service,
which runs `/system/bin/install-recovery.sh` once per boot. SuperSU's
`/system/su.d` doesn't work there: its daemon runs those scripts only
after `supolicy` sets `supolicy.loaded`, and this firmware never runs
`supolicy`.

The change: `/system/bin/raam-boot.sh` (root, 0750) and one line
appended to `install-recovery.sh` calling it.

```sh
#!/system/bin/sh
# Wi-Fi is turned back on at every boot, so a power cycle undoes "Wi-Fi off".
n=0
while [ "$(getprop sys.boot_completed)" != 1 ] && [ $n -lt 300 ]; do
  sleep 2
  n=$((n + 2))
done
if [ "$(settings get global wifi_on)" = 1 ]; then
  log -t raam-boot "Wi-Fi already on"
else
  log -t raam-boot "Wi-Fi was off at boot, turning it on"
  svc wifi enable
fi
```

Install, with the script at `/data/local/tmp/raam-boot.sh`:

```sh
adb shell su -c 'mount -o rw,remount /system'
adb shell su -c 'cp /data/local/tmp/raam-boot.sh /system/bin/raam-boot.sh && chmod 750 /system/bin/raam-boot.sh'
adb shell "su -c 'printf \"\n# Raam boot script (raam docs/FRAME-SETUP.md)\n/system/bin/raam-boot.sh\n\" >> /system/bin/install-recovery.sh'"
adb shell su -c 'mount -o ro,remount /system'
```

Undo: remove the two added lines and the file, the same way. Keep a
copy of the stock `install-recovery.sh` before editing it. Its own
part logs `Installing new recovery image: failed` at every boot on
the SNUG; that is the stock script, and it writes nothing (the
recovery partition stays byte-identical).

## Not part of a setup

Lab experiments installed while Raam was being built (`rs.frame.*`)
aren't needed; `rs.frame.video` is disabled on the first frame and the
rest are inert.
