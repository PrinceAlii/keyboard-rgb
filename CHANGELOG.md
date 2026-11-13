# Changelog

## 0.1.0 - 2026-11-13

- Added hardware-tested wired AULA F99 lighting with 99-key painting, Static/Rainbow/Wave/Breathing, brightness/speed controls and volatile HID streaming with change detection and a 1 s keepalive.
- Added an egui GUI with live keyboard preview, saved lighting state, mouse controls and diagnostics.
- Added a Settings tab for a per-user start-with-Windows entry, config and backup folder shortcuts, and a copy confirmation in Diagnostics.
- Added hardware-tested Razer Viper Mini polling, DPI and logo-brightness reads and guarded writes. Static colour remains experimental and preview-only in recorded testing; failure-triggered rollback and power-cycle retention remain unverified.
- Added CLI probes, config backups/diffs, guarded keyboard mode/config operations, lighting tests and mouse setter previews.
- Added conflict detection for known lighting programs and additional rgbd/rgbctl processes.
- Added repository documentation, contributing guidance and MIT/Apache-2.0 license texts.
