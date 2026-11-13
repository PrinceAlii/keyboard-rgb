# Razer Viper Mini protocol

Updated 2026-11-11: polling, DPI and logo-brightness identity/changed writes
are verified on this unit. See [RAZER_WRITE_TESTS.md](RAZER_WRITE_TESTS.md)
for the recorded tests and artifacts. Other write recipes remain untested.

## Implementation status (2026-11-11)

- Strict framing accepts exactly a 90-byte body or 91 bytes with report ID
  `0x00`; any other length/ID or declared size above 80 is rejected. Parsing
  retains `crc_ok` for diagnostics; `send_and_read` enforces CRC validity,
  transaction ID, class and command echoes for both reads and writes.
- Read response sizes must match requests exactly, as confirmed by raw hardware
  captures. Write response sizes are bounded to 80 but equality is **not yet
  enforced**, because raw write responses have not been captured. Status handling
  remains in `require_success`, allowing discovery to inspect unsupported status.
- `Response::payload()` exposes only declared argument bytes. Read getters
  validate sizes before indexing; DPI-stage parsing rejects counts above five
  or missing entries. Its coordinate-only API still discards stage identifiers
  and reserved bytes, so it is insufficient for exact profile rollback.
- Brightness reads and writes using `0x3F` are hardware-confirmed. Standard
  polling and DPI writes using the discovered `0xFF` are also confirmed.
  This does not verify static colour, other write commands or lighting TX `0xFF`.
- Guarded polling/DPI/brightness setters perform readback and one rollback
  attempt on failure. They support an identity write but do not automatically
  run one before changing a value. The tested CLI changed-value runs performed
  a verified identity preflight before the target. Error-triggered rollback
  remains untested on hardware; successful return-to-baseline writes are not
  evidence of the error-path rollback behavior.
- Public `poll_rate_to_byte`, `dpi_write_args`, `brightness_write_args` and
  `static_write_args` support exact CLI previews without changing encodings.
  The CRC comment now matches the implementation; the module's layout table
  uses **90-byte body offsets**, not HIDAPI buffer offsets.
- Static colour has no documented colour readback or rollback. It cannot
  satisfy the full guarded-setter policy; keep it an explicitly acknowledged
  experimental operation, preview-only under the strict policy.
- One rapid reopen failed in discovery with `HidD_SetFeature` semaphore
  timeout `0x00000079`, before any brightness setter. The retry succeeded;
  recommend bounded retry/backoff around device open, or a short settle delay,
  for rapid CLI invocations. A reopen/settle issue is the working explanation,
  not a proven root cause; do not blindly retry uncertain completed writes.

## Device and control path

### GUI worker (2026-11-11)

The Mouse tab sends commands to a separate worker holding the Razer handle;
it does not route mouse commands through the AULA engine. Changed polling,
DPI and brightness requests run a guarded identity preflight before the target.
Generation tags reject commands queued while disconnected or before a completed
reconnect. Every connected command is followed by a full snapshot; a failed
snapshot clears the connection and schedules another open attempt after 1 s.
There is no periodic snapshot while connected and idle: use Refresh for changes
made outside the app. Transient semaphore open errors get one 750 ms retry.

Static colour requires a fresh confirmation checkbox and explicit Write colour
click; the UI passes `accepted: true`, and the worker rejects false acceptance.
The confirmation stays visible across tab switches and resets its checkbox if
the connection changes; it is disabled while unavailable/busy. It does not
verify colour or provide rollback. Power-cycle DPI persistence remains untested.
Busy events are balanced by an RAII guard, but carry no request identifier;
pending UI requests can race an older idle event. Shutdown saves settings before
joining the worker, so an in-flight synchronous HID call can delay close but
cannot lose the saved lighting state.

### HID selection

- Wired AULA companion mouse: Razer Viper Mini, USB `1532:008A`.
- Confirmed control collection: `MI_00`, interface `0`, usage page `0x0001`,
  usage `0x0002`. Use HID feature reports, report ID `0x00`.
- `MI_01` collections failed `HIDD_SetFeature` with "Incorrect function".
  Future writes must target only the confirmed collection and device identity.
- Reads succeeded with transaction ID `0xFF`. Current transport sends a
  request, waits 10 ms, then fetches a feature response; this delay is not a
  guarantee of completion. Serialize request/response pairs.
- `src/hid.rs` correctly separates mouse enumeration from AULA `258A:010C`.
  Its `0x06`/520-byte constants describe AULA, not Razer.

## Report layout and CRC

All offsets below include HIDAPI's leading report-ID byte; total length is 91.

| Buffer byte(s) | Meaning |
| --- | --- |
| `0` | Report ID `0x00` |
| `1` | Status: request `0x00`; response `0x02` success, `0x05` unsupported |
| `2` | Transaction ID |
| `3..4` | Remaining packets, big-endian; `00 00` for these requests |
| `5` | Protocol `0x00` |
| `6` | Data size, at most 80 bytes |
| `7` | Command class |
| `8` | Command ID; bit 7 set for read commands |
| `9..88` | 80 argument bytes; unused bytes zero-filled |
| `89` | XOR of bytes `3..=88` |
| `90` | Reserved `0x00` |

CRC excludes report ID, status, transaction ID, CRC and reserved byte. In a bare
90-byte body, subtract one from every buffer offset: CRC is body byte 88,
XOR over body bytes `2..=87`. All `args[n]` below mean buffer byte `9+n`.
Read size declares the expected payload even when request arguments are empty.

## Confirmed read commands and observed values

These reads succeeded with transaction ID `0xFF`; brightness also succeeded
with `0x3F`, which `logo_brightness()` now uses. Remaining request arguments
were zero. `NOSTORE=0x00`, `VARSTORE=0x01`, `LOGO_LED=0x04`.

| Read | Class / ID / size | Request args | Response args / observed value |
| --- | --- | --- | --- |
| Firmware | `00 / 81 / 02` | none | `[0]` major, `[1]` minor: **v1.3** |
| Serial | `00 / 82 / 16` (22 bytes) | none | `[0..22)` string: device serial (redacted) |
| Mode | `00 / 84 / 02` | none | `[0]`: **00**, hardware mode |
| Polling | `00 / 85 / 01` | none | `[0]`: **01**, 1000 Hz |
| DPI | `04 / 85 / 07` | `[00]` | X=`[1], [2]`, Y=`[3], [4]`, big-endian: **800 x 800** |
| DPI stages | `04 / 86 / 26` (38 bytes) | `[01]` | `[0]` storage, `[1]` active stage, `[2]` count; five stages **400/800/1100/1400/1700** |
| Logo brightness | `0F / 84 / 03` | `[01,04]` | `[2]`: **FF**, 255/255 |

Stages start at `args[3]`, with seven bytes each:
`[stage, x_hi, x_lo, y_hi, y_lo, 00, 00]`. Preserve actual stage identifiers;
do not infer them from display row numbers. Hardware captured indices **1..5**.
The final probe reports active stage **2**. Every earlier raw read
returned status `0x02`, matching transaction/class/command/declared-size echoes
and valid CRC. These observations are from the reported hardware captures;
the write-test artifacts also record a successful final read-only probe.

## Write commands

Hex values below are exact; `b`, `r`, `g`, `x_hi`, etc. are input bytes.
Zero-fill the rest of the report. No alternative command classes are implied.

| Write | Class / ID / size | Exact argument payload | TX | Persistence / verification / rollback |
| --- | --- | --- | --- | --- |
| Polling | `00 / 05 / 01` | `[p]`, `p=01` for 1000 Hz, `02` for 500, `08` for 125 | `FF` | Persistence across power cycles unverified; get `00/85`, restore saved rate on mismatch |
| DPI | `04 / 05 / 07` | `[01,x_hi,x_lo,y_hi,y_lo,00,00]` | `FF` | VARSTORE requests onboard storage; power-cycle persistence unconfirmed. Get `04/85`, compare both axes; restore saved X/Y on mismatch, mandatory |
| DPI stages | `04 / 06 / 26` | `[01,active,count]` then seven-byte entries as above, zero padding to 38 bytes | `FF` | Onboard profile; untested. Save complete original payload, read `04/86`, compare all meaningful fields and restore original on mismatch |
| Brightness | `0F / 04 / 03` | `[01,04,b]` | `3F` | VARSTORE requests onboard storage; power-cycle retention unverified. Get `0F/84` with `[01,04]`, compare `[2]`, restore saved brightness on mismatch |
| Static colour | `0F / 02 / 09` | `[01,04,01,00,00,01,r,g,b]` | `3F` | VARSTORE requests onboard storage; retention unverified. Validate acknowledgement and visually inspect; no documented colour readback/rollback |
| Mode | `00 / 04 / 02` | `[mode,00]`, known normal/driver values `00`/`03` | `FF` (read-proven ID; mode write untested) | Persistence unverified. Read `00/84`, preserve original two-byte mode payload and restore on mismatch. Not needed for first tests; never use factory mode `02` |

DPI axes are big-endian; 800=`03 20`, 900=`03 84`. Reject values above
8500; minimum supported DPI and granularity need confirmation. Do not clamp
inputs without reporting the change or substitute a `NOSTORE` DPI write: this
recipe uses VARSTORE, but unplug/replug persistence has not been tested.

## Verified write results (2026-11-11)

Times below are artifact filename timestamps, local `+11:00`. Class/command/
size, TX and arguments specify the request; report ID is `00`, remaining
fields/padding are zero and CRC follows the layout above. Each row passed
acknowledgement validation and target readback, exit `0`.

| Artifact time | Operation: previous -> requested = readback | TX | Class / command / size (hex) | Target args |
| --- | --- | --- | --- | --- |
| 21:15:17 | Poll identity: 1000 -> 1000 | `FF` | `00 / 05 / 01` | `[01]` |
| 21:15:17 | DPI identity: 800x800 -> 800x800 | `FF` | `04 / 05 / 07` | `[01 03 20 03 20 00 00]` |
| 21:16:49 | Brightness identity retry: 255 -> 255 | `3F` | `0F / 04 / 03` | `[01 04 FF]` |
| 21:17:25 | Poll: 1000 -> 500 | `FF` | `00 / 05 / 01` | `[02]` |
| 21:17:37 | Poll: 500 -> 1000 | `FF` | `00 / 05 / 01` | `[01]` |
| 21:17:47 | DPI: 800x800 -> 900x900 | `FF` | `04 / 05 / 07` | `[01 03 84 03 84 00 00]` |
| 21:17:57 | DPI: 900x900 -> 800x800 | `FF` | `04 / 05 / 07` | `[01 03 20 03 20 00 00]` |
| 21:18:09 | Brightness: 255 -> 128 | `3F` | `0F / 04 / 03` | `[01 04 80]` |
| 21:18:17 | Brightness: 128 -> 255 | `3F` | `0F / 04 / 03` | `[01 04 FF]` |

Each changed-value invocation also identity-wrote and read back its previous
value first. The initial brightness identity attempt in the 21:15:17 artifact
failed during open/discovery, exit `1`, before a brightness set report; it is
not a failed brightness command. No raw write response bytes were saved:
CRC/transaction/class/command/status checks passed, but response-size echo is
unobserved and unenforced. Final 21:18:30 probe confirms return to baseline: hardware mode
`00`, 1000 Hz, 800x800 DPI, active stage 2, five unchanged stages, brightness 255.
No power-cycle persistence or failure-triggered rollback was tested.

### Untested custom path

TX `0x3F` for both packets:

1. Select effect: class `0x0F`, ID `0x02`, size `0x0C`, args
   `[00,00,08,00,00,00,00,00,00,00,00,00]`.
2. Frame: class `0x0F`, ID `0x03`, size `0x47` (71), args initially zero;
   `[2]=row`, `[3]=start_col`, `[4]=stop_col`, RGB triples from `[5]`.
   Row/column mapping is unverified; do not guess coordinates or send frames.

Custom-frame persistence and restoration of the previous effect are unverified;
there is no documented frame readback. This is not a first-test write path.

## Safety policy

1. Run the read-only probe first and bind the validated identity/control path.
   Read queries use feature-report sends but do not request setting changes.
2. Validate every response before consuming values: exact supported framing,
   report ID when present, CRC, transaction echo, class, command, expected read
   size, maximum write size, success status and command-specific fields/ranges.
   Write-size equality awaits captured write responses. Reject unsupported,
   busy, failed, malformed or mismatched responses; never treat them as values.
3. Save original settings before any write. For each readable setting, **write
   its current value first**, then read it back. Stop if this identity write
   fails; only then attempt a changed value. Preserve full stage payloads.
4. Validate the write acknowledgement and verify by readback. On mismatch or
   uncertain completion after a write, attempt restoration using the saved
   original and verify that restoration. Report rollback failure explicitly
   and stop further changes; avoid unbounded retries of persistent writes.
5. Static colour cannot perform the current-value write/readback/rollback
   sequence. Keep it preview-only under the strict policy; an eventual hardware
   test requires explicit acceptance of that limitation and visual observation.
6. No guessed commands, transaction IDs, mode switches or frame coordinates.
   Leave the AULA keyboard's signal, config, timing and lighting untouched;
   Razer commands must never open/write its collection.
7. SignalRGB UI/launcher must stay stopped and its service disabled; do not
   restore startup. The established AULA fix depends on this
   ([ROOT_CAUSE.md](ROOT_CAUSE.md)). Exclude competing mouse writers as well.

## Proposed CLI surface

CLI surface (polling/DPI/brightness exercised; static preview only):

```text
rgbctl razer-probe --raw
rgbctl razer-set poll <125|500|1000> [--dry-run]
rgbctl razer-set dpi <x> [y] [--dry-run]
rgbctl razer-set brightness <0-255> [--dry-run]
rgbctl razer-set static "<#RRGGBB>" [--dry-run]
```

Omitted Y means X. Preview should perform validated reads and print identity,
current/target values, complete proposed reports including TX/CRC, persistence
and rollback plan, without sending setter packets (including identity writes).
Static preview must state that the current colour is unavailable and rollback
is unsupported. The recorded CLI requires `--accept-unverified --yes` for a
real static write; this does not satisfy strict verification policy. Defer
mode, stages and custom-frame CLI writes.

First hardware test sequence **completed 2026-11-11**: probe;
identity-write/readback each setting; polling
**1000 -> 500 -> 1000**; DPI **800x800 -> 900x900 -> 800x800**; brightness
**255 -> 128 -> 255**. Every target and identity preflight was verified.
For future tests, verify every step and stop/restore on failure. Static remains
optional and **not run**: only with its limitation explicitly accepted may one
static colour (for example `"#00FF00"`) be tested and visually inspected.

## Open questions / not yet verified items

- Custom effect/frame support, coordinates, persistence and restoration on this
  exact unit. The local SignalRGB custom-selection helper uses a different
  payload tail; do not substitute it for the supplied untested recipe.
- DPI stages writes, active-stage/index semantics, and exact rollback behavior;
  retain original entry identifiers rather than reconstructing them.
- Static colour writes, mode writes and button remapping remain untested.
  Colour/effect readback and rollback have no documented commands supplied.
- Raw write response sizes remain uncaptured; size equality is not enforced.
  Failure-triggered rollback and recovery behavior remain hardware-unverified.
- Whether `0xFF` also works for lighting writes: unknown; use `0x3F`.
- Power-cycle retention, supported DPI minimum/step, and whether DPI writes
  change any stage/profile fields beyond the current X/Y setting.
  Final readback confirms stage values stayed unchanged in these tests.

## References

- [OpenRazer razermouse_driver.c](https://github.com/openrazer/openrazer/blob/master/driver/razermouse_driver.c):
  device-specific dispatch and transaction selection.
- [OpenRazer razerchromacommon.c](https://github.com/openrazer/openrazer/blob/master/driver/razerchromacommon.c):
  standard mode, polling/DPI and extended matrix packet builders. These are
  reference recipes; they do not establish write success on this hardware.
- Local SignalRGB `Razer_Modern_Mouse.js`:
  `%LOCALAPPDATA%\WhirlwindFX\SignalRgb\cache\plugin_cdn\main\Plugins\Razer\Razer_Modern_Mouse.js`.
  Inspected packet/CRC wrapper, transaction discovery, DPI and brightness
  helpers. Its default/scanned IDs do not override the supplied `0x3F`
  lighting-write requirement. Read this file without running SignalRGB.
- Repository audit: `src/razer.rs`, `src/hid.rs`, `src/bin/rgbctl.rs`,
  [ROOT_CAUSE.md](ROOT_CAUSE.md), [DIAGNOSIS_BRIEF.md](DIAGNOSIS_BRIEF.md);
  observed hardware values and exact write facts supplied with this task.
- [Write-test log and evidence artifact list](RAZER_WRITE_TESTS.md).
