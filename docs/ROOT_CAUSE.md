# AULA F99 flicker — root cause and resolution

Date: 2026-11-11
Status: **resolved and verified on hardware**

## Root cause

SignalRGB was running a second writer against the same AULA F99 HID
collection. Its installed SinoWealth plugin identified the board as
**AULA F99** and sent the same volatile stream command as `rgbd`:

```text
report 06 08 00 00 01 00 7A 01  + 378-byte interleaved RGB payload
```

There is no cross-process lock in the firmware. SignalRGB and `rgbd` alternated
frames, so the board looked like it was flickering, blinking, or dropping keys.
This also explains why the symptom rate changed with our send cadence: the
board was showing whichever writer painted last.

## Evidence

- `SignalRgb.exe` (PID 36668), `SignalRgbLauncher.exe`, and
  `SignalRgbService` were running during the flicker tests.
- SignalRGB's log contains an active render plugin named **AULA F99**, and
  records it reinitializing at **2026-11-11 18:35**, during testing.
- The installed plugin
  `%LOCALAPPDATA%\WhirlwindFX\SignalRgb\cache\plugin_cdn\main\Plugins\Sinowealth\Sinowealth_Keyboard_Controller.js`
  contains the identical `0x08` sender, a 378-byte frame builder, the same
  520-byte feature report, and the F99 model entry (`0xA4`).
- With SignalRGB stopped, a 30 s solid red stream at 20 FPS was **steady red,
  no flicker** (user-observed).
- With SignalRGB stopped and the new change-only + 1 s keepalive mode, a 25 s
  solid red stream was also **steady red, no flicker**.
- In the GUI, **Rainbow, Wave, and Breathing are all smooth** with SignalRGB
  stopped.

## Resolution

1. Closed the SignalRGB UI and launcher.
2. Removed the `SignalRgb` value from
   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
3. Stopped and disabled the `SignalRgb.Service` service. Current state:
   `Status=Stopped`, `StartType=Disabled`.
4. Added a competing-process monitor (`src/conflicts.rs`). The GUI scans for
   SignalRGB, OpenRGB, the AULA vendor app, `rgbd.exe`, and `rgbctl.exe` every
   two seconds and shows a red contention banner instead of letting the two
   programs overwrite each other without a warning.
5. Changed the streaming policy to **send on frame change, then refresh an
   unchanged frame once per second** (`KEEPALIVE_MS = 1000`). `0x08` is a
   volatile framebuffer: when the stream stops, the board reverts to its stored
   effect. A 1 s keepalive keeps a static colour solid without writing USB at
   20 FPS forever.
6. Animations still send changed frames at the 50 ms pacing floor (20 FPS).
7. `rgbctl solid` now defaults to the same change-only + keepalive policy. Its
   `--continuous` flag is diagnostic only and exists to reproduce the noisy
   every-frame behavior if needed.

## Restore SignalRGB (if ever wanted)

The removed startup value is backed up at
`%APPDATA%\rgbd\backups\signalrgb-startup-restore.reg`. Restoring the service
requires an elevated shell:

```powershell
Set-Service -Name 'SignalRgb.Service' -StartupType Automatic
Start-Service -Name 'SignalRgb.Service'
```

Do not run SignalRGB while `rgbd` owns the keyboard; whichever program writes
last wins.

## Remaining protocol facts

- `0x08` payload is 378 bytes: 126 interleaved RGB slots.
- The current stored mode bytes are `01 15 47`. This unit renders host-streamed
  per-key colours correctly with them; no further config writes are needed.
- OpenRGB's working F99 driver also streams `0x08` and uses a roughly 1 s
  keepalive; it does not write the mode config.
- The direct `0x06` static path is still avoided: repeated writes can blank the
  board.
- A USB capture of the vendor app has not been taken. USBPcap is not installed;
  Wireshark/TShark 4.2.6 are present.
