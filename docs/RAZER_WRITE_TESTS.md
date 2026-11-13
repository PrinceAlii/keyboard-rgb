# Razer Viper Mini write tests

Date: **2026-11-11**, local timezone `+11:00` (Australia/Sydney).
Evidence reviewed from `%APPDATA%\rgbd\backups\razer`; no hardware commands
were run while preparing this log. Times below come from artifact filenames,
not individual packet timestamps. See [RAZER_PROTOCOL.md](RAZER_PROTOCOL.md)
for report framing and safety policy.

## Device and pre-write state

- Viper Mini `1532:008A`, firmware **v1.3**. The device serial was recorded in
  the local evidence and is redacted here.
- Control: interface **0**, usage page **0x0001**, usage **0x0002**, discovered
  TX **0xFF** for standard/DPI requests; brightness reads/writes use **0x3F**.
- Confirmed Windows control path:

```text
\\?\HID#VID_1532&PID_008A&MI_00#<instance>#{4d1e55b2-f16f-11cf-88cb-001111000030}
```

The environment snapshot explicitly records
`2026-11-11T21:09:34.9092662+11:00`. Its process list contains only `rgbd.exe`,
PID **7164**, from this project's `target\release` directory. SignalRGB was
absent from that list and `SignalRgb.Service` was **Stopped / Disabled**.
**Razer Game Manager Service 3 was Running / Automatic**; this was not a test
with every Razer service disabled. The Viper Mini PnP entries reported `OK`.

At **21:13:03**, dry runs previewed polling 1000, DPI 800x800, brightness 255
and static `#00ff00`; each explicitly logged no set report sent and exit `0`.
Identity-preview CRCs were `05`, `07`, `F2` respectively; the static preview
was TX `3F`, class `0F`, command `02`, size `09`, args
`[01 04 01 00 00 01 00 FF 00]`, CRC `FE`. Static was not actually written.

The **21:13:35** post-dry-run probe established the pre-write baseline:
mode `00` hardware, **1000 Hz**, **800x800 DPI**, active stage **2**,
five stages **400/800/1100/1400/1700** on both axes, brightness **255/255**.
Firmware and serial matched the recorded device identity.

## Writes and readback

Class, command, size, TX and argument bytes are hex. Sizes `01`, `07`, `03`
mean 1, 7, 3 argument bytes. These are request encodings from the confirmed
recipes/previews; the execution logs record outcomes, not raw USB packets.
In the values column, `previous / requested / readback` uses decimal units
(Hz, DPI XxY or brightness). `I` is an identity write; `T` is a target change.
All rows below passed; every containing invocation logged **EXIT=0**.

| Artifact time | Phase / setting | TX | Class/cmd/size | Args | Previous / requested / readback | Result |
| --- | --- | --- | --- | --- | --- | --- |
| 21:15:17 | I poll | `FF` | `00/05/01` | `[01]` | 1000 / 1000 / 1000 | Pass |
| 21:15:17 | I DPI | `FF` | `04/05/07` | `[01 03 20 03 20 00 00]` | 800x800 / 800x800 / 800x800 | Pass |
| 21:16:49 | I brightness (retry) | `3F` | `0F/04/03` | `[01 04 FF]` | 255 / 255 / 255 | Pass |
| 21:17:25 | I poll preflight | `FF` | `00/05/01` | `[01]` | 1000 / 1000 / 1000 | Pass |
| 21:17:25 | T poll | `FF` | `00/05/01` | `[02]` | 1000 / 500 / 500 | Pass |
| 21:17:37 | I poll preflight | `FF` | `00/05/01` | `[02]` | 500 / 500 / 500 | Pass |
| 21:17:37 | T poll | `FF` | `00/05/01` | `[01]` | 500 / 1000 / 1000 | Pass |
| 21:17:47 | I DPI preflight | `FF` | `04/05/07` | `[01 03 20 03 20 00 00]` | 800x800 / 800x800 / 800x800 | Pass |
| 21:17:47 | T DPI | `FF` | `04/05/07` | `[01 03 84 03 84 00 00]` | 800x800 / 900x900 / 900x900 | Pass |
| 21:17:57 | I DPI preflight | `FF` | `04/05/07` | `[01 03 84 03 84 00 00]` | 900x900 / 900x900 / 900x900 | Pass |
| 21:17:57 | T DPI | `FF` | `04/05/07` | `[01 03 20 03 20 00 00]` | 900x900 / 800x800 / 800x800 | Pass |
| 21:18:09 | I brightness preflight | `3F` | `0F/04/03` | `[01 04 FF]` | 255 / 255 / 255 | Pass |
| 21:18:09 | T brightness | `3F` | `0F/04/03` | `[01 04 80]` | 255 / 128 / 128 | Pass |
| 21:18:17 | I brightness preflight | `3F` | `0F/04/03` | `[01 04 80]` | 128 / 128 / 128 | Pass |
| 21:18:17 | T brightness | `3F` | `0F/04/03` | `[01 04 FF]` | 128 / 255 / 255 | Pass |

Successful writes passed strict framing, CRC, transaction/class/command echoes
and success status handling, followed by validated readback. The supplied
operator account confirms these checks; raw write responses were not saved,
so their exact bytes and declared-size echoes cannot be independently inspected.
Write-size equality remains unenforced (maximum 80 still applies).

## Semaphore timeout and recovery

The first brightness identity attempt in the **21:15:17** artifact followed
the successful DPI identity command and exited **1** during device discovery:

```text
HidD_SetFeature: (0x00000079) The semaphore timeout period has expired.
```

The control collection opened but failed its firmware query, before any
brightness set report. The final discovery error reports attempted TX `9F`;
other collections reported `Incorrect function`. This is not evidence of an
invalid brightness command or an applied wrong setting.

The operator reports a successful read-only recovery probe about three seconds
later with settings intact; that intermediate probe is **not included in the
listed artifacts**, so its exact time cannot be substantiated here. The
**21:16:49** artifact directly confirms successful brightness identity retry.
A transient reopen/settle issue is the working explanation; root cause was
not isolated. Recommend bounded retry/backoff around device open, or a short
settle delay between rapid CLI invocations. Do not replay a setter blindly
after uncertain completion.

## Final state and limits

The **21:18:30** read-only probe confirms firmware v1.3, the recorded serial,
hardware mode `00`, **1000 Hz**, **800x800 DPI**, active stage
**2**, unchanged five stages **400/800/1100/1400/1700**, brightness **255/255**.
The operator reports probe exit `0`; the saved probe text records completion
but does not print an `EXIT=` line. The pre-write probe similarly records
completion without an exit-code line.

Still unverified: unplug/replug persistence (including DPI despite successful
VARSTORE acknowledgement/readback), failure-triggered rollback, 125 Hz writes,
DPI-stage writes, custom frames, mode writes, button remapping and raw write
response size. No automatic rollback was triggered in these tests; returning
to baseline was an explicit successful write. Static colour remains unwritten,
without documented colour readback/rollback, and preview-only under strict
policy; a future experiment requires explicit acceptance of that limitation.

## Evidence artifacts

Base: `%APPDATA%\rgbd\backups\razer`.

```text
env-preflight-20261111-210934.txt
dry-runs-20261111-211303.txt
post-dryrun-probe-20261111-211335.txt
identity-writes-20261111-211517.txt
brightness-identity-20261111-211649.txt
poll-1000-to-500-20261111-211725.txt
poll-500-to-1000-20261111-211737.txt
dpi-800-to-900-20261111-211747.txt
dpi-900-to-800-20261111-211757.txt
brightness-255-to-128-20261111-211809.txt
brightness-128-to-255-20261111-211817.txt
post-write-probe-20261111-211830.txt
```
