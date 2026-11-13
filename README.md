# rgbd

`rgbd` is a Windows 10/11 application that controls an AULA F99 keyboard and a Razer Viper Mini mouse directly over HID, without the vendor apps. It includes an egui GUI (`rgbd.exe`) and a command-line tool (`rgbctl.exe`). Connect both devices over wired USB.

This project is early-stage and tested only on the following hardware. Other AULA and Razer models are not supported.

| Device | USB VID:PID | Tested support |
| --- | --- | --- |
| AULA F99 | `258A:010C` | Keyboard lighting, including per-key streaming |
| Razer Viper Mini | `1532:008A` | Reads and guarded polling, DPI and logo-brightness writes |

Mouse static colour is experimental and has only been previewed, not written in the recorded hardware tests. It cannot be read back or rolled back.

## GUI features

- Paint individual keys on the 99-key layout, with a live keyboard preview.
- Select Static, Rainbow, Wave or Breathing; adjust brightness and animation speed.
- Save the painted frame and lighting settings automatically in `%APPDATA%\rgbd\state.ini`.
- Detect competing SignalRGB, OpenRGB, AULA vendor app, `rgbctl`, and second `rgbd` processes. Detection shows a warning; it does not lock the device or stop other programs.
- Read the Viper Mini firmware, serial, mode, polling rate, DPI, DPI stages and logo brightness. Apply guarded polling, DPI and brightness changes from the Mouse tab.
- Use Settings to toggle start with Windows and open the config or backups folder. The Diagnostics tab copies a text report.

Remap is a placeholder. Mouse DPI-stage editing and button remapping are not implemented.

## Download

Tagged releases contain a Windows x86-64 zip with `rgbd.exe`, `rgbctl.exe`, the licenses, and these docs. Releases are on the [GitHub releases page](https://github.com/PrinceAlii/keyboard-rgb/releases). There is no installer; extract the zip. The executables are unsigned, so Windows may show a SmartScreen warning.

## Build from source

Use Windows with Rust 1.95 or newer (edition 2024) and Visual Studio Build Tools with the C++ workload, including MSVC and a Windows SDK. Run from the repository root in a Developer PowerShell or a shell with the MSVC environment loaded:

```powershell
cargo build --release
```

The binaries are `target\release\rgbd.exe` and `target\release\rgbctl.exe`. If MSVC tools are missing from your shell environment, use the optional helper:

```powershell
. .\scripts\env.ps1
cargo build --release
```

The helper finds the installed C++ toolchain through `vswhere.exe` and imports `vcvars64.bat` into the current PowerShell session. It does not install build tools. It can also run a command directly:

```powershell
.\scripts\env.ps1 cargo build --release
```

## Run the GUI

Close competing lighting programs before starting the GUI; it begins streaming the current keyboard frame when connected.

```powershell
.\target\release\rgbd.exe
```

Choose an effect in Lighting or paint keys in Static mode. Mouse settings are read on connection; use Refresh after changes made outside the app. DPI writes request onboard storage, but unplug/replug persistence has not been tested.

## CLI probes and previews

These commands read device state without changing device settings. `config` and `led-map` save local backup files. HID read queries still send feature reports to request responses.

```powershell
.\target\release\rgbctl.exe --help
.\target\release\rgbctl.exe probe
.\target\release\rgbctl.exe config
.\target\release\rgbctl.exe led-map
.\target\release\rgbctl.exe config-diff
.\target\release\rgbctl.exe razer-probe --raw
```

`config-diff` compares the two newest config backups by default. To compare specific backups, pass both paths as positional arguments. `config` and `led-map` accept `--backup-dir`.

Mouse dry runs read current values and print the target report and real-run plan. They send no setter reports:

```powershell
.\target\release\rgbctl.exe razer-set poll 500 --dry-run
.\target\release\rgbctl.exe razer-set dpi 900 900 --dry-run
.\target\release\rgbctl.exe razer-set brightness 128 --dry-run
.\target\release\rgbctl.exe razer-set static "#00ff00" --dry-run
```

Polling accepts 125, 500 or 1000 Hz; only 500 and 1000 Hz writes have recorded hardware tests. DPI accepts X and optional Y (omitting Y uses X for both), with both axes validated in `100..=8500`; the supported hardware minimum and step are not established. Brightness accepts `0..=255`.

## CLI writes

Run only one device writer at a time. Close the GUI before streaming from the CLI. These examples **change device state**:

```powershell
.\target\release\rgbctl.exe solid --color "#ff0000" --seconds 5 --fps 20
.\target\release\rgbctl.exe test --seconds-per-color 2 --no-sweep
.\target\release\rgbctl.exe razer-set poll 500
.\target\release\rgbctl.exe razer-set dpi 900
.\target\release\rgbctl.exe razer-set brightness 128
```

`solid --seconds 0` runs until interrupted; `--continuous` forces repeated frames for diagnosis and is not needed for a steady colour. `test` includes a single-LED sweep unless `--no-sweep` is supplied; `--sweep-hold-ms` controls its hold time.

Mouse setters prompt for confirmation unless `--yes` is supplied. For a changed readable setting, the CLI first writes and verifies the current value, then attempts the target. The guarded setter reads back the target and attempts one restoration on failure. Failure-triggered rollback has unit tests but has not been exercised on hardware.

Keep static colour preview-only under the documented safety policy. The CLI has an experimental escape hatch requiring both `--accept-unverified` and `--yes`; acknowledgement and unchanged brightness do not verify the colour. An error may leave a new colour applied.

## Keyboard protocol and stored mode

Keyboard lighting uses volatile command `0x08` frames in 520-byte HID feature reports, report ID `0x06`. Each frame contains 378 bytes: 126 interleaved RGB slots, with 99 mapped keys. The GUI and default `solid` stream send on change, with a 50 ms minimum gap (20 FPS ceiling) and a 1 s keepalive for unchanged frames. When streaming stops, the keyboard returns to its stored effect.

Stored mode bytes are separate from streamed colours. On the tested F99, config offsets `[9, 10, 59]` contain `01 15 47` and per-key streaming works. **The tested keyboard is already in per-key mode; no config write is needed.** This tuple is not established for every firmware batch.

`per-key-mode` and `restore-config` write **non-volatile configuration**. They are recovery or deliberate setup operations, not routine startup steps. Review the device's own config and backup before considering them:

```powershell
.\target\release\rgbctl.exe per-key-mode --dry-run
.\target\release\rgbctl.exe restore-config --help
```

`per-key-mode` reads and backs up the device's config, patches only the three mode bytes, then verifies readback and attempts restoration on verification failure. `restore-config <path>` saves the current config before restoring a validated 128-byte backup; it has no dry-run flag. Both accept `--backup-dir` and `--yes`; restore also has `--force-file` to override the backup filename check. Do not substitute another keyboard's config. The persistent static command `0x06` is avoided because repeated writes can blank the board.

## Device write safety

Close SignalRGB (including its launcher/service), OpenRGB, the AULA vendor app, and second copies of rgbd before streaming. Two writers cause flicker or partial-looking key updates because the firmware displays whichever frame arrived last. Do not run vendor software while rgbd owns the device. Close Razer Synapse and other mouse writers before applying mouse settings.

Conflict detection is a process-name warning, not exclusive ownership. It does not enumerate every possible application or service. Read the [flicker investigation](docs/ROOT_CAUSE.md) before changing stream timing or stored mode to address contention.

Mouse static colour cannot be read back, saved as an original colour, or rolled back. Recorded testing covers polling, DPI and brightness acknowledgement/readback only; power-cycle persistence and failure-triggered restoration remain unverified. See the [mouse protocol policy](docs/RAZER_PROTOCOL.md) and [write-test evidence](docs/RAZER_WRITE_TESTS.md).

## Troubleshooting

| Symptom | Checks and fixes |
| --- | --- |
| Flicker, blinking or partial keys | Close competing writers, including background SignalRGB processes/services and another GUI or CLI stream. Check the contention banner. Keep the 50 ms floor; faster writes do not solve contention. |
| Status says `Effect mode` | The connected device did not report the documented per-key mode tuple. Close writers, run `probe` and `config`, and inspect the mode bytes. This is a stored-mode hint, not proof of failed streaming. The tested board uses `01 15 47`; do not run `per-key-mode` as a routine fix. |
| Keyboard not found | Select wired USB mode, check the cable, and run `probe`. The tool needs `258A:010C` and a responding vendor-page `0xFF00` collection identified by valid config reads or the F99 model query. Other models and wireless modes are unsupported. The GUI retries after disconnects. |
| Mouse not found | Check the wired connection and `1532:008A`, close mouse-control software, then use `razer-probe --raw` or Refresh. The confirmed control collection is interface 0, usage page `0x0001`, usage `0x0002`. After a semaphore timeout during discovery, allow a short settle period and retry a read-only probe; do not replay an uncertain setter blindly. |
| New commands or UI are missing | Close the running app, rebuild with `cargo build --release`, and launch the binary from this checkout's `target\release`. A copied executable or a binary in another checkout can be stale. |

## Documentation and contributions

- [Architecture and module map](docs/ARCHITECTURE.md)
- [Diagnosis brief](docs/DIAGNOSIS_BRIEF.md)
- [Keyboard flicker root cause](docs/ROOT_CAUSE.md)
- [Razer protocol and write policy](docs/RAZER_PROTOCOL.md)
- [Razer hardware write tests](docs/RAZER_WRITE_TESTS.md)
- [Contributing](CONTRIBUTING.md)
- [Changelog](CHANGELOG.md)

## License and affiliation

Copyright (c) 2026 Ali B. Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

Not affiliated with AULA or Razer.
