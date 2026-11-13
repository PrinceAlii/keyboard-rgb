# AULA F99 lighting flicker — diagnosis brief

Date: 2026-11-11
Project: `rgbd` (Rust + egui, Windows)
Goal: native per-key lighting for the AULA F99, no vendor app, no OpenRGB runtime.

> **Resolved 2026-11-11.** The flicker was SignalRGB streaming the same volatile
> `0x08` frames to the same HID collection. With SignalRGB stopped, both 20 FPS
> and change-only + 1 s keepalive are steady, and all animated GUI effects are
> smooth. See [ROOT_CAUSE.md](ROOT_CAUSE.md). The original diagnosis trail below
> is kept for context; its "current code state" and speculative causes are
> superseded by the resolution.

## Hardware and transport

- Keyboard: AULA F99, wired USB. USB ID `258A:010C` (SinoWealth).
- RGB collection: `MI_01&Col06`, usage page `0xFF00`, usage `0x0001`.
- Transport: HID **feature reports**, report ID `0x06`, 520 bytes total.
- Packet header: `[0x06, command, addr(4), len_le(2)]`, payload at offset 8.
- Address for colour/config: `00 00 01 00`.
- Model query `0x82`; response byte 13 = `0xA4` (F99). First reply can be stale.
- Config read `0x84`, 128 bytes, valid when it ends `5A A5`. Reads can be corrupt while busy; require two identical reads.
- Config write `0x04`, 128 bytes, non-volatile. This project has now done exactly one guarded write.

## Colour commands (from keylux PROTOCOL.md, F75 but same family)

| Purpose | Cmd | Length | Layout | Slots |
| --- | --- | --- | --- | --- |
| Static per-key, persists | `0x06` | `0x0180` (384) | planar: 128 R, 128 G, 128 B | 128 |
| Live streaming | `0x08` | `0x017A` (378) | interleaved RGB | 126 |
| Solid single colour | `0x0A` | `0x0200` (512) | interleaved | 170 |

- Keylux documents static `0x06` as 384 bytes. An earlier version of this brief
  said 378; that is the `0x08` payload length, not `0x06`.
- Channel order is RGB, not GRB.
- LED indices are the keyboard's key-matrix order; this project's layout uses indices 0-112 for 99 physical keys.
- Firmware/key-scan floor is ~44.7 ms between colour writes. The vendor streams at ~21.5 FPS. This project uses 50 ms / 20 FPS.
- Repeated `0x06` writes can blank the board. `0x08` is the animation path.

## Mode bytes

Config offsets `[9] [10] [59]` select the lighting mode.

- F75 documented per-key: `01 15 47`
- F75 documented solid: `00 01 40`
- F99 config when this investigation began: `00 10 47`
- After our guarded read-modify-write: `01 15 47` (only bytes 9 and 10 changed; byte 59 was already `47`).
- Readback matched the written block exactly, and typing still works.

The F99's own 128-byte config differs from the F75 block after byte ~25, but the first bytes and mode-byte positions look analogous. The F99 effect table contains `04 47` entries where the F75 block has `09 47`.

## Symptom timeline (user-observed on the physical keyboard)

1. **Before any config write**, board mode `00 10 47`, streaming a solid frame at 20 FPS: lighting flashed randomly, did not stay one colour. This was attributed to a firmware effect repainting over host frames.
2. **After switching to `01 15 47`**:
   - `rgbctl test --no-sweep` cycled red/green/blue/white for 3 s each (continuous 20 FPS), then exited. User later reported **no lights on at all** after the CLI exited. Typing works.
   - GUI streaming with **change-only updates + ~900 ms keepalive**: solid cyan flickers roughly every 1.5 s; sometimes the whole board, sometimes only a few keys on one side.
   - GUI changed to **continuous 20 FPS** (50 ms): solid cyan now flickers **very fast, whole keyboard**.
   - Stopping `rgbd` leaves the board dark.
3. **Resolved:** SignalRGB was driving the same collection with the same `0x08`
   frames. After closing it, a 30 s solid red stream at 20 FPS was steady, and
   the new change-only + 1 s keepalive policy was also steady for 25 s. GUI
   Rainbow, Wave, and Breathing were all smooth. See [ROOT_CAUSE.md](ROOT_CAUSE.md).

The user is available to watch the keyboard, but cannot provide electrical measurements. We can run experiments.

## Key reference facts (post-resolution)

- OpenRGB `SinowealthKeyboard10cController` sends only `0x08` (520-byte feature report, 378 payload) and never sets mode; its device table maps the F99 (`0xA4`). It uses a roughly 1 s keepalive because the keyboard reverts out of direct mode when packets stop.
- keylux is F75-specific. Its docs say `0x08` is the animation path and host colours render only in per-key mode. It skips identical frames and refreshes at a 2 s keepalive.
- keylux also warns a config write triggers an asynchronous repaint that overwrites frames for ~400 ms.
- SignalRGB's installed Sinowealth plugin contained the exact same `0x08` packet builder and the F99 model entry, which is why it could fight this driver.
- The F99 vendor app lives under `C:\Program Files (x86)\AULA\F99`. Its `KB.ini` has the key layout and effect table, but static analysis found no literal `0x08`/config block. Do not run the vendor app while we own the device.
- USBPcap is not installed. Wireshark/TShark 4.2.6 are present but have no USB capture interface. A vendor capture is not needed for lighting anymore.
- The stored mode bytes on this unit are `01 15 47`; they render host-streamed per-key colours correctly. Public sources do not independently validate that tuple for every F99 firmware batch, but no further config write is justified on this unit.

## Current code state

- `src/sinowealth.rs`: packet builders (`build_packet`, `stream_packet`), config read/write, mode helpers (`mode_of`, `with_mode`, `MODE_PER_KEY = [01 15 47]`), `Keyboard::send_stream`.
- `src/engine.rs`: background HID worker. Sends on frame change, then refreshes an unchanged frame at `KEEPALIVE_MS` (1 s), with a 50 ms floor for changed frames. Repaints immediately after a reconnect.
- `src/conflicts.rs`: scans the process table every 2 s for SignalRGB, OpenRGB, the AULA vendor app, a second `rgbd.exe`, and `rgbctl.exe`.
- `src/bin/rgbctl.rs`: `probe`, `config`, `led-map`, `config-diff`, `per-key-mode` (guarded config write), `restore-config`, `solid` (change-only by default; `--continuous` diagnostic flag), `test`.
- `src/app.rs`: egui GUI, 99-key preview, Static/Rainbow/Wave/Breathing, contention banner/status, state in `%APPDATA%\rgbd\state.ini`.
- Backups: `%APPDATA%\rgbd\backups\aula-f99-*-config.bin`, including `aula-f99-1794383394-pre-mode-config.bin` (original `00 10 47`).
- SignalRGB: autostart Run value removed (backup: `%APPDATA%\rgbd\backups\signalrgb-startup-restore.reg`); `SignalRgb.Service` is `Stopped` and `Disabled`.
- Reference files: `%TEMP%\rgbd-ref\keylux-protocol.md`, `keylux-protocol.rs`, `keylux-f75-mod.rs`, `openrgb-10c.cpp`, `openrgb-10c.h`, `devices.cpp`.

## Constraints

- Do not write config bytes or send unknown commands to the keyboard as part of diagnosis.
- Do not use or depend on the AULA vendor app at runtime.
- Any config write must be a read-modify-write of this keyboard's own block, signature-checked, backed up, and read back.
- Keep the 50 ms floor unless evidence says otherwise.
