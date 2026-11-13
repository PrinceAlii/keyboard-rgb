# Architecture

`rgbd` has two executable entry points sharing the `rgbd` library. Hardware access uses `hidapi`; the GUI uses eframe/egui. Supported device IDs are fixed in `hid` rather than selected from arbitrary HID devices.

## Module map

| Module | Responsibility |
| --- | --- |
| `src/app.rs` | GUI tabs, keyboard painting, effect generation, preview, status, diagnostics and lighting-state persistence. Settings contains start-with-Windows and config/backups folder controls; Diagnostics has the copy button. Remap remains a placeholder. |
| `src/engine.rs` | Keyboard worker, connection/reconnection, latest-frame handling, stream pacing and status channel. |
| `src/hid.rs` | HID enumeration and collection opening; separate filters for AULA `258A:010C` and Razer `1532:008A`. AULA uses vendor page `0xFF00`. |
| `src/sinowealth.rs` | F99 packet builders, model/config/LED-map queries, config validation and mode helpers, and volatile RGB stream sending. |
| `src/razer.rs` | Razer report builders, CRC, response validation, discovery, getters and guarded setters. |
| `src/mouse.rs` | Mouse worker, connection generations, snapshots, busy/result events and identity preflight before changed settings. |
| `src/layout.rs` | 99 F99 key rectangles and LED indices derived from the vendor layout. |
| `src/conflicts.rs` | Background process scan every two seconds; reports known competing writers without terminating them or locking HID. |
| `src/color.rs` | RGB value and hex parsing/formatting. |
| `src/bin/rgbd.rs` | Native window setup and `RgbdApp` entry point. |
| `src/bin/rgbctl.rs` | Clap CLI, read-only probes, config backup/diff/restore, guarded mode changes, keyboard streams and mouse previews/setters. |
| `src/lib.rs` | Shared module exports. |

## Threading and device ownership

The egui thread owns editing state and draws the preview. It sends frames through an mpsc channel to `rgbd-hid`; only that worker holds the keyboard handle. The worker drains queued frames to retain the newest frame, reports connection status, and retries connection after one second when unavailable. HID calls may block the worker but do not run on the repaint thread. Reconnection resets the last-sent frame so the latest colours are resent.

A separate `rgbd-mouse` worker holds the Razer handle and serializes mouse requests and responses. The UI sends commands and polls status, busy and result events. Connection-generation tags reject requests submitted while disconnected or before a reconnect. Each connected command is followed by a full snapshot. While connected and idle there is no periodic snapshot; Refresh requests a new one. Transient semaphore errors during open have one 750 ms retry. Writes are not blindly retried.

`rgbd-conflicts` runs the process monitor and publishes lists over another channel. The GUI drains older conflict reports. Detection is advisory: firmware provides no cross-process writer lock. The CLI performs synchronous operations and warns about competing processes before keyboard streaming.

On GUI shutdown, lighting state is saved before stopping/joining device workers. An in-flight synchronous HID operation can delay close.

## Keyboard frame flow

1. `app` builds a 126-slot RGB frame from the painted colours or the selected Static/Rainbow/Wave/Breathing effect, then applies brightness. `layout` maps the 99 keys to slots.
2. The preview draws that frame; the UI also submits it to `Engine`.
3. The worker sends changed frames no faster than every 50 ms (20 FPS maximum). An unchanged frame is refreshed at a 1 s keepalive.
4. `sinowealth::stream_packet` encodes 378 interleaved RGB bytes in a 520-byte feature report with report ID `0x06`, command `0x08` and header `06 08 00 00 01 00 7A 01`.
5. `Keyboard::send_stream` sends the report to the opened AULA collection.

`0x08` updates a volatile framebuffer. Stopping the stream returns the board to its stored effect. It does not persist the painted colours or change the stored mode. Persistent static command `0x06` is not used.

The 128-byte config ends in signature `5A A5`. Mode offsets `[9, 10, 59]` are `01 15 47` on the tested unit, which already streams per-key colours correctly. `Effect mode` in the UI means those bytes did not match the documented tuple; it is a hint based on the config read.

## Guarded write policy

Keyboard config writes belong to explicit CLI operations, not the lighting worker. `per-key-mode` reads the device's own block, backs it up, patches only the three mode bytes, writes, waits for settling and verifies readback. Verification failure triggers an attempt to restore the original. `restore-config` checks the input length/signature and backup filename (unless overridden), checks model identity, and saves the current block before writing the replacement. These are non-volatile operations, not startup requirements.

Razer buffers are 91 bytes including report ID `0x00`, or 90-byte response bodies. Validation checks supported framing, CRC, transaction/class/command echoes and success status before values are used. Read sizes must equal the requested size; write sizes are bounded to 80 but equality is not enforced pending raw write captures. Standard/DPI requests use the discovered transaction ID (`0xFF` on the tested unit); logo brightness and static writes use `0x3F`.

Polling, DPI and logo-brightness setters save the current value, validate inputs, write and verify readback. Once a write starts, any failure leads to at most one restoration attempt and a readback of the original setting; errors report restoration results. The GUI worker and CLI first identity-write/read back the current value before a changed target. A failed preflight aborts the target.

DPI and brightness packets request VARSTORE; unplug/replug retention remains untested. Polling/DPI/brightness changes are hardware-tested, but failure-triggered rollback is only unit-tested. Static colour has acknowledgement and brightness checks, with no colour readback or rollback. It remains experimental and preview-only under the strict policy, despite an explicitly acknowledged write path in the GUI/CLI. See [Razer protocol](RAZER_PROTOCOL.md) and [hardware tests](RAZER_WRITE_TESTS.md).

## Config and backups

| Path | Contents |
| --- | --- |
| `%APPDATA%\rgbd\state.ini` | GUI painted frame, selected colour, effect, brightness and speed; automatically saved after edits and at shutdown. |
| `%APPDATA%\rgbd\backups` | CLI keyboard config backups named `aula-f99-*-config.bin`, plus raw LED maps. `--backup-dir` can override the output directory for supported commands. |
| `%APPDATA%\rgbd\backups\razer` | Recorded mouse test artifacts described in the write-test log; not an automatic per-write logging guarantee. |

The Settings tab exposes config/backups folder access and a per-user start-with-Windows entry under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. The Diagnostics tab can copy a text report. State and backup helpers fall back to the current directory as their base if `APPDATA` is absent.

## Protocol and worker tests

`cargo test` runs unit tests without device writes. `sinowealth` tests report headers, model-query bytes, interleaved stream payload/length, config signatures, mode-patch isolation and LED-map parsing. `layout` tests 99 unique LED indices, bounds and known key mappings.

`razer` tests report layout and CRC, response framing/report ID/size rejection, echo checks, polling encodings, big-endian DPI arguments, brightness/static payloads and DPI-stage parsing. Closure-driven guarded-write tests cover success, identity writes, mismatch, failed acknowledgements/readback and the single restoration limit. These tests simulate responses; they do not prove hardware recovery.

`mouse` tests input validation, preflight ordering, abort/no-retry behavior, connection-generation rejection and busy/error event handling without opening HID. `conflicts` tests Windows tasklist CSV parsing. Hardware observations are recorded separately in [ROOT_CAUSE.md](ROOT_CAUSE.md) and [RAZER_WRITE_TESTS.md](RAZER_WRITE_TESTS.md).
