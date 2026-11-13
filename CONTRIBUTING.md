# Contributing

The supported hardware is the wired AULA F99 (`258A:010C`) and Razer Viper Mini (`1532:008A`) on Windows 10/11. Other models require device-specific evidence before support can be claimed.

## Build and checks

Use Rust 1.95+ with edition 2024 support and Visual Studio Build Tools with the C++ workload and Windows SDK. From a Developer PowerShell, run:

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

If needed, load the MSVC environment with `. .\scripts\env.ps1` first. The release executables are in `target\release`. Unit tests cover encodings and simulated failures; running the GUI starts keyboard streaming and is a hardware operation.

## Code layout

`src/app.rs` owns egui editing and display. `engine` owns the keyboard worker, while `mouse` owns the separate Razer worker. `hid` enumerates and opens collections. `sinowealth` and `razer` implement device protocols. `layout` maps keys; `color` handles RGB values; `conflicts` detects known competing processes. Executable entry points are in `src/bin`.

Read the [architecture](docs/ARCHITECTURE.md), [keyboard investigation](docs/ROOT_CAUSE.md) and [Razer protocol policy](docs/RAZER_PROTOCOL.md) before changing hardware behavior.

## Protocol safety for hardware changes

- Keep HID access off the GUI thread and serialize each request/response pair. Keep mouse traffic separate from the AULA engine and device filter.
- Use evidence for command bytes, collection identity, transaction IDs, payload lengths and persistence. Do not probe unknown setters, guess mode switches, or infer support from a shared vendor ID.
- Preserve the keyboard's 50 ms frame floor and change-only plus 1 s keepalive policy unless observed evidence justifies a change. Use volatile `0x08` for lighting; do not introduce persistent `0x06` streaming.
- Before a non-volatile keyboard config write, validate stable reads and signature, back up that device's original block, limit changed bytes, and verify readback. Keep restoration failures visible. Never copy another unit's config.
- For readable mouse settings, save the original, identity-write/read back before changing it, validate acknowledgements and target readback, then attempt one verified restoration on failure. Do not retry uncertain writes in a loop.
- Preserve strict response validation. Static colour has no documented colour readback or rollback; keep it preview-only under the strict policy. Experimental hardware writes require explicit acceptance of this limit and visual observation.
- Close competing writers before any hardware test, including SignalRGB background components, OpenRGB, vendor apps and other rgbd/rgbctl instances.

Hardware tests must be observed and documented. Record device identity and firmware, exact commands/report bytes, pre-write values, requested values, readback, visible behavior, errors and final state. Distinguish unit-test results, previews, observed writes, restoration tests and unplug/replug persistence. See the [mouse test log](docs/RAZER_WRITE_TESTS.md) for the current evidence and its limits.

## Adding a device

Start with read-only enumeration and protocol research for the exact VID:PID, firmware and HID collection. Supply captures or an identified reference implementation; do not guess commands from a related model. Establish report framing, response validation, supported reads and persistence before proposing writes.

Add an explicit device filter and protocol implementation or a justified device-specific dispatch, with encoding/validation tests. Document verified collection selection and a safe observed test plan with original-state backup and restoration. Keep unsupported operations unavailable or marked experimental until observed hardware tests establish them. A matching packet builder alone is not proof of device support.

## Commits and pull requests

Keep commits focused and use a message that states the change. In the PR, describe the concrete problem, resulting behavior, affected devices and checks run. Include hardware evidence for protocol changes and state what remains unverified. Update user commands and protocol notes when behavior changes. Do not commit personal backups, serial-number captures, build output or unrelated formatting changes.
