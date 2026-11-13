//! Native Razer Viper Mini (`1532:008A`) control protocol.
//!
//! Razer devices expose a 90-byte feature report (report ID `0x00`):
//!
//! ```text
//! offset  size  field
//! 0       1     status (response only)
//! 1       1     transaction ID
//! 2       2     remaining packets, big-endian (always 0)
//! 4       1     protocol type (always 0)
//! 5       1     data size (argument bytes)
//! 6       1     command class
//! 7       1     command ID (bit 7 set = device -> host)
//! 8       80    arguments
//! 88      1     CRC (XOR of bytes 2..=87)
//! 89      1     reserved
//! ```
//!
//! HIDAPI prepends the report ID, so every buffer in this module is 91 bytes.
//!
//! Guarded setters verify polling rate, DPI, and brightness by readback and
//! attempt one rollback if verification fails. Static colour has no documented
//! readback and cannot be restored to the previous colour.

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use hidapi::HidDevice;

use crate::hid::{HidCandidate, HidManager};

/// Unnumbered feature report used by Razer's configuration protocol.
pub const REPORT_ID: u8 = 0x00;

/// Number of bytes HIDAPI moves in each direction, report ID included.
pub const BUFFER_LEN: usize = 91;
/// The protocol body without HIDAPI's report-ID byte.
pub const BODY_LEN: usize = 90;
/// First argument byte inside the 91-byte buffer.
pub const ARG_OFFSET: usize = 9;
/// Maximum number of argument bytes the report can carry.
pub const MAX_ARGS: usize = 80;
/// CRC byte inside the 91-byte buffer.
pub const CRC_OFFSET: usize = 89;

/// Status byte returned by a successful command.
pub const STATUS_SUCCESS: u8 = 0x02;
/// Status byte returned when the command is not supported.
pub const STATUS_NOT_SUPPORTED: u8 = 0x05;

/// Persist the setting in the device's onboard profile.
pub const VARSTORE: u8 = 0x01;
/// Apply the setting immediately without touching onboard storage.
pub const NOSTORE: u8 = 0x00;

/// The Viper Mini's single LED zone (its logo).
pub const LOGO_LED: u8 = 0x04;

/// Transaction IDs to try while identifying the device.
///
/// OpenRazer uses `0xFF` for the Viper Mini, while SignalRGB scans
/// `0x1F..0x9F`. The device appears to echo whichever ID it is given, so we
/// verify a real response instead of assuming one.
pub const TRANSACTION_IDS: [u8; 10] = [0xFF, 0x1F, 0x3F, 0x2F, 0x4F, 0x5F, 0x6F, 0x7F, 0x8F, 0x9F];

/// Transaction ID required by the Viper Mini's matrix lighting commands.
pub const LIGHTING_TRANSACTION_ID: u8 = 0x3F;

/// Viper Mini's documented DPI ceiling.
pub const MAX_DPI: u16 = 8500;

/// A Razer command class.
pub mod class {
    pub const STANDARD: u8 = 0x00;
    pub const LED: u8 = 0x03;
    pub const DPI: u8 = 0x04;
    pub const MATRIX: u8 = 0x0F;
}

/// Razer command IDs. Reads have bit 7 set.
pub mod cmd {
    // Standard device commands (class 0x00).
    pub const SET_MODE: u8 = 0x04;
    pub const GET_FIRMWARE: u8 = 0x81;
    pub const GET_SERIAL: u8 = 0x82;
    pub const GET_MODE: u8 = 0x84;
    pub const SET_POLL_RATE: u8 = 0x05;
    pub const GET_POLL_RATE: u8 = 0x85;

    // DPI commands (class 0x04).
    pub const SET_DPI: u8 = 0x05;
    pub const GET_DPI: u8 = 0x85;
    pub const SET_DPI_STAGES: u8 = 0x06;
    pub const GET_DPI_STAGES: u8 = 0x86;

    // Extended matrix lighting (class 0x0F).
    pub const SET_BRIGHTNESS: u8 = 0x04;
    pub const GET_BRIGHTNESS: u8 = 0x84;
    pub const SET_EFFECT: u8 = 0x02;
    pub const SET_CUSTOM_FRAME: u8 = 0x03;
}

/// Build a request report.
///
/// `data_size` is the number of argument bytes the command carries or expects
/// back. It is deliberately separate from `args.len()`: Razer's read commands
/// declare the response size (22 for the serial number, 38 for DPI stages)
/// while sending few or no arguments.
pub fn build_report(
    transaction_id: u8,
    command_class: u8,
    command_id: u8,
    data_size: u8,
    args: &[u8],
) -> [u8; BUFFER_LEN] {
    assert!(args.len() <= MAX_ARGS, "Razer report arguments overflow");
    assert!(
        usize::from(data_size) <= MAX_ARGS,
        "Razer data size exceeds the report"
    );

    let mut report = [0u8; BUFFER_LEN];
    report[0] = REPORT_ID;
    report[2] = transaction_id;
    report[6] = data_size;
    report[7] = command_class;
    report[8] = command_id;
    report[ARG_OFFSET..ARG_OFFSET + args.len()].copy_from_slice(args);
    report[CRC_OFFSET] = crc(&report);
    report
}

/// Razer's CRC: XOR report bytes `3..=88` (remaining packets through arguments).
/// Excludes report ID, status, transaction ID, CRC byte and reserved byte.
pub fn crc(report: &[u8; BUFFER_LEN]) -> u8 {
    report[3..=88].iter().fold(0, |acc, byte| acc ^ byte)
}

/// One decoded response.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u8,
    pub transaction_id: u8,
    pub data_size: u8,
    pub class: u8,
    pub command: u8,
    pub args: Vec<u8>,
    pub crc_ok: bool,
    raw: Vec<u8>,
}

impl Response {
    pub fn is_success(&self) -> bool {
        self.status == STATUS_SUCCESS
    }

    pub fn arg(&self, index: usize) -> Option<u8> {
        self.args.get(index).copied()
    }

    /// The declared argument payload, excluding unused report padding.
    /// Parsed responses guarantee `data_size <= 80` and contain 80 argument bytes.
    pub fn payload(&self) -> &[u8] {
        &self.args[..usize::from(self.data_size)]
    }

    fn require_payload(&self, expected_size: u8) -> Result<&[u8]> {
        if self.data_size != expected_size {
            bail!(
                "response payload size mismatch: expected {expected_size}, actual {}",
                self.data_size
            );
        }
        if usize::from(self.data_size) > self.args.len() {
            bail!(
                "response payload truncated: expected {} bytes, actual {}",
                self.data_size,
                self.args.len()
            );
        }
        Ok(self.payload())
    }

    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Human-readable status for diagnostics.
    pub fn status_name(&self) -> &'static str {
        status_name(self.status)
    }
}

/// Decode a response from a 91-byte HIDAPI buffer or a bare 90-byte body.
pub fn parse_response(raw: &[u8]) -> Result<Response> {
    let body: &[u8] = match raw.len() {
        BODY_LEN => raw,
        BUFFER_LEN => {
            if raw[0] != REPORT_ID {
                bail!(
                    "Razer report ID mismatch: expected 0x{REPORT_ID:02X}, actual 0x{:02X}",
                    raw[0]
                );
            }
            &raw[1..]
        }
        actual => bail!(
            "Razer response length mismatch: expected {BODY_LEN} or {BUFFER_LEN} bytes, actual {actual}"
        ),
    };

    if usize::from(body[5]) > MAX_ARGS {
        bail!(
            "Razer response data size exceeds maximum: expected <= {MAX_ARGS}, actual {}",
            body[5]
        );
    }

    let crc_ok = body[2..88].iter().fold(0, |acc, byte| acc ^ byte) == body[88];

    Ok(Response {
        status: body[0],
        transaction_id: body[1],
        data_size: body[5],
        class: body[6],
        command: body[7],
        args: body[8..88].to_vec(),
        crc_ok,
        raw: body.to_vec(),
    })
}

pub fn status_name(status: u8) -> &'static str {
    match status {
        STATUS_SUCCESS => "command successful",
        0x01 => "device busy",
        0x03 => "command failure",
        0x04 => "command timed out",
        STATUS_NOT_SUPPORTED => "command not supported",
        _ => "unknown status",
    }
}

pub fn mode_name(mode: u8) -> &'static str {
    match mode {
        0x00 => "hardware mode",
        0x02 => "factory mode",
        0x03 => "software mode",
        _ => "unknown mode",
    }
}

/// Decode the standard polling-rate byte.
pub fn poll_rate_from_byte(value: u8) -> Option<u16> {
    match value {
        0x01 => Some(1000),
        0x02 => Some(500),
        0x08 => Some(125),
        _ => None,
    }
}

/// Encode a polling rate of 125, 500, or 1000 Hz.
pub fn poll_rate_to_byte(hz: u16) -> Result<u8> {
    match hz {
        1000 => Ok(0x01),
        500 => Ok(0x02),
        125 => Ok(0x08),
        _ => bail!("unsupported polling rate {hz} Hz; expected 125, 500, or 1000 Hz"),
    }
}

/// Validate both DPI axes (100-8500) and encode an onboard-profile write.
pub fn dpi_write_args(x: u16, y: u16) -> Result<[u8; 7]> {
    if !(100..=MAX_DPI).contains(&x) || !(100..=MAX_DPI).contains(&y) {
        bail!("DPI axes must each be in 100..={MAX_DPI}; requested x={x}, y={y}");
    }

    let [x_hi, x_lo] = x.to_be_bytes();
    let [y_hi, y_lo] = y.to_be_bytes();
    Ok([VARSTORE, x_hi, x_lo, y_hi, y_lo, 0, 0])
}

/// Encode logo brightness (0-255) for the onboard profile.
pub fn brightness_write_args(brightness: u8) -> [u8; 3] {
    [VARSTORE, LOGO_LED, brightness]
}

/// Encode a static logo colour for the onboard profile.
pub fn static_write_args(red: u8, green: u8, blue: u8) -> [u8; 9] {
    [VARSTORE, LOGO_LED, 0x01, 0x00, 0x00, 0x01, red, green, blue]
}

/// Values observed before and after a successfully verified write.
#[derive(Clone, Debug)]
pub struct WriteOutcome<T> {
    pub previous: T,
    pub requested: T,
    pub readback: T,
}

/// DPI stages as reported by `GET_DPI_STAGES`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DpiStages {
    pub active_stage: u8,
    pub count: u8,
    /// `(x, y)` pairs in stage order.
    pub stages: Vec<(u16, u16)>,
}

/// Parse the 38-byte DPI-stages response payload.
///
/// Layout: varstore, active stage, stage count, then 7 bytes per stage:
/// stage index, X big-endian, Y big-endian, two reserved bytes.
/// This API retains coordinates only; stage writes must preserve identifiers
/// and reserved bytes separately when implemented.
pub fn parse_dpi_stages(args: &[u8]) -> Result<DpiStages> {
    if args.len() < 3 {
        bail!("DPI-stage response is too short: {} bytes", args.len());
    }

    let active_stage = args[1];
    let count = args[2];
    if count > 5 {
        bail!("DPI-stage count exceeds maximum: expected <= 5, actual {count}");
    }
    let parsed = usize::from(count);
    let required = 3 + parsed * 7;
    if required > args.len() {
        bail!(
            "DPI-stage payload truncated: expected at least {required} bytes for {count} stages, actual {}",
            args.len()
        );
    }

    let mut stages = Vec::with_capacity(parsed);
    for stage in 0..parsed {
        let base = 3 + stage * 7;
        let x = u16::from_be_bytes([args[base + 1], args[base + 2]]);
        let y = u16::from_be_bytes([args[base + 3], args[base + 4]]);
        stages.push((x, y));
    }

    Ok(DpiStages {
        active_stage,
        count,
        stages,
    })
}

/// One open Razer device.
pub struct Device {
    dev: HidDevice,
    pub path: String,
    pub product: String,
    pub usage_page: u16,
    pub usage: u16,
    pub interface: i32,
    pub transaction_id: u8,
}

impl Device {
    /// Find the first Viper Mini collection that answers a real read command.
    ///
    /// Interface 0 (the mouse collection, usage page 1 / usage 2) is tried
    /// first because that is the interface OpenRazer and SignalRGB use. The
    /// vendor collections on interface 1 are tried next.
    pub fn open(manager: &HidManager) -> Result<Self> {
        let mut candidates = manager.razer_collections();
        if candidates.is_empty() {
            bail!(
                "no Razer Viper Mini HID collections found (expected {:04X}:{:04X})",
                crate::hid::RAZER_VID,
                crate::hid::VIPER_MINI_PID
            );
        }
        candidates.sort_by_key(candidate_rank);

        let mut failures = Vec::new();
        for candidate in &candidates {
            match Self::open_candidate(manager, candidate) {
                Ok(device) => return Ok(device),
                Err(err) => failures.push(format!(
                    "iface={} usage_page=0x{:04X} usage=0x{:04X}: {err:#}",
                    candidate.interface, candidate.usage_page, candidate.usage
                )),
            }
        }

        bail!(
            "no Viper Mini collection answered the Razer protocol:\n  {}",
            failures.join("\n  ")
        )
    }

    /// Open one collection and identify the transaction ID with a read-only
    /// firmware query.
    pub fn open_candidate(manager: &HidManager, candidate: &HidCandidate) -> Result<Self> {
        let dev = manager.open(candidate)?;
        let mut last_error = None;

        for transaction_id in TRANSACTION_IDS {
            match send_and_read(
                &dev,
                transaction_id,
                class::STANDARD,
                cmd::GET_FIRMWARE,
                2,
                &[],
            ) {
                Ok(response) if response.is_success() => {
                    return Ok(Self {
                        dev,
                        path: candidate.path_display.clone(),
                        product: candidate.product.clone(),
                        usage_page: candidate.usage_page,
                        usage: candidate.usage,
                        interface: candidate.interface,
                        transaction_id,
                    });
                }
                Ok(response) => {
                    last_error = Some(anyhow!(
                        "transaction 0x{transaction_id:02X}: {} (status 0x{:02X})",
                        response.status_name(),
                        response.status
                    ));
                }
                Err(err) => {
                    last_error = Some(anyhow!("transaction 0x{transaction_id:02X}: {err:#}"));
                }
            }
        }

        bail!(
            "collection opened but no firmware response: {}",
            last_error
                .map(|err| format!("{err:#}"))
                .unwrap_or_else(|| "no transaction ID answered".to_string())
        )
    }

    /// Send one command and read its response.
    pub fn request(
        &self,
        command_class: u8,
        command_id: u8,
        data_size: u8,
        args: &[u8],
    ) -> Result<Response> {
        self.request_with_transaction_id(
            self.transaction_id,
            command_class,
            command_id,
            data_size,
            args,
        )
    }

    /// Send a command using the transaction ID required by its command family.
    fn request_with_transaction_id(
        &self,
        transaction_id: u8,
        command_class: u8,
        command_id: u8,
        data_size: u8,
        args: &[u8],
    ) -> Result<Response> {
        send_and_read(
            &self.dev,
            transaction_id,
            command_class,
            command_id,
            data_size,
            args,
        )
    }

    /// Firmware version, as `(major, minor)`.
    pub fn firmware(&self) -> Result<(u8, u8)> {
        let response = self.request(class::STANDARD, cmd::GET_FIRMWARE, 2, &[])?;
        require_success(&response, "firmware")?;
        let payload = response.require_payload(2)?;
        Ok((payload[0], payload[1]))
    }

    /// Device serial number.
    pub fn serial(&self) -> Result<String> {
        let response = self.request(class::STANDARD, cmd::GET_SERIAL, 22, &[])?;
        require_success(&response, "serial number")?;

        let bytes: Vec<u8> = response
            .require_payload(22)?
            .iter()
            .copied()
            .take_while(|byte| *byte != 0)
            .collect();
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    /// Current device mode (`0x00` hardware, `0x03` software).
    pub fn mode(&self) -> Result<u8> {
        let response = self.request(class::STANDARD, cmd::GET_MODE, 2, &[])?;
        require_success(&response, "device mode")?;
        Ok(response.require_payload(2)?[0])
    }

    /// Current polling rate in Hz. `None` means the byte is not a value this
    /// driver knows, not that the command failed.
    pub fn poll_rate(&self) -> Result<(u8, Option<u16>)> {
        let response = self.request(class::STANDARD, cmd::GET_POLL_RATE, 1, &[])?;
        require_success(&response, "polling rate")?;
        let raw = response.require_payload(1)?[0];
        Ok((raw, poll_rate_from_byte(raw)))
    }

    /// Current DPI as `(x, y)`.
    pub fn dpi(&self) -> Result<(u16, u16)> {
        let response = self.request(class::DPI, cmd::GET_DPI, 7, &[NOSTORE])?;
        require_success(&response, "DPI")?;
        let payload = response.require_payload(7)?;
        Ok((
            u16::from_be_bytes([payload[1], payload[2]]),
            u16::from_be_bytes([payload[3], payload[4]]),
        ))
    }

    /// Current onboard DPI stages.
    pub fn dpi_stages(&self) -> Result<DpiStages> {
        let response = self.request(class::DPI, cmd::GET_DPI_STAGES, 38, &[VARSTORE])?;
        require_success(&response, "DPI stages")?;
        parse_dpi_stages(response.require_payload(38)?)
    }

    /// Current logo brightness (0-255).
    pub fn logo_brightness(&self) -> Result<u8> {
        let response = self.request_with_transaction_id(
            LIGHTING_TRANSACTION_ID,
            class::MATRIX,
            cmd::GET_BRIGHTNESS,
            3,
            &[VARSTORE, LOGO_LED],
        )?;
        require_success(&response, "logo brightness")?;
        Ok(response.require_payload(3)?[2])
    }

    /// Set polling to 125, 500, or 1000 Hz and verify it by readback.
    ///
    /// Invalid rates are rejected before USB traffic. The write is sent even
    /// when the rate is unchanged. A failed write or verification triggers one
    /// rollback attempt, whose write and readback results are included in the error.
    pub fn set_poll_rate_guarded(&self, hz: u16) -> Result<WriteOutcome<u16>> {
        poll_rate_to_byte(hz)?;
        let read = || {
            let (raw, hz) = self.poll_rate()?;
            hz.with_context(|| format!("unsupported polling rate byte 0x{raw:02X}"))
        };
        let previous = read().context("read polling rate before guarded write")?;

        guarded_write(
            "polling rate (Hz)",
            previous,
            hz,
            |value| {
                let args = [poll_rate_to_byte(value)?];
                let response = self.request(class::STANDARD, cmd::SET_POLL_RATE, 1, &args)?;
                require_success(&response, "set polling rate")
            },
            read,
        )
    }

    /// Persist DPI to the onboard profile and verify both axes by readback.
    ///
    /// Each axis must be in `100..=8500`; invalid requests are rejected before
    /// USB traffic. The write is sent even when DPI is unchanged. A failed write
    /// or verification triggers one rollback attempt to the previous DPI, with
    /// its write and readback results included in the error. Rollback can fail.
    pub fn set_dpi_guarded(&self, x: u16, y: u16) -> Result<WriteOutcome<(u16, u16)>> {
        dpi_write_args(x, y)?;
        let previous = self.dpi().context("read DPI before guarded write")?;
        dpi_write_args(previous.0, previous.1)
            .context("current DPI cannot be safely restored; refusing to write")?;

        guarded_write(
            "DPI",
            previous,
            (x, y),
            |(x, y)| {
                let args = dpi_write_args(x, y)?;
                let response = self.request(class::DPI, cmd::SET_DPI, 7, &args)?;
                require_success(&response, "set DPI")
            },
            || self.dpi(),
        )
    }

    /// Persist logo brightness (0-255) and verify it by readback.
    ///
    /// The write is sent even when brightness is unchanged. A failed write or
    /// verification triggers one rollback attempt, whose write and readback
    /// results are included in the error. Lighting uses transaction ID `0x3F`.
    pub fn set_logo_brightness_guarded(&self, brightness: u8) -> Result<WriteOutcome<u8>> {
        let previous = self
            .logo_brightness()
            .context("read logo brightness before guarded write")?;

        guarded_write(
            "logo brightness",
            previous,
            brightness,
            |value| {
                let args = brightness_write_args(value);
                let response = self.request_with_transaction_id(
                    LIGHTING_TRANSACTION_ID,
                    class::MATRIX,
                    cmd::SET_BRIGHTNESS,
                    3,
                    &args,
                )?;
                require_success(&response, "set logo brightness")
            },
            || self.logo_brightness(),
        )
    }

    /// Persist a static logo colour and check that brightness is unchanged.
    ///
    /// The colour change is not readback-verifiable and is not reversible:
    /// there is no documented colour readback, so the previous colour cannot
    /// be saved or rolled back. Success confirms only the command acknowledgement
    /// and unchanged brightness. An error may leave the new colour applied.
    pub fn set_logo_static(&self, red: u8, green: u8, blue: u8) -> Result<()> {
        let previous = self
            .logo_brightness()
            .context("read logo brightness before static colour write")?;
        let args = static_write_args(red, green, blue);
        let response = self
            .request_with_transaction_id(
                LIGHTING_TRANSACTION_ID,
                class::MATRIX,
                cmd::SET_EFFECT,
                9,
                &args,
            )
            .context("static colour write failed; the colour cannot be rolled back")?;
        require_success(&response, "set static logo colour")
            .context("the colour cannot be rolled back")?;
        thread::sleep(Duration::from_millis(20));
        let readback = self.logo_brightness().context(
            "read brightness after static colour write; the colour cannot be rolled back",
        )?;
        if readback != previous {
            bail!(
                "static logo colour changed brightness: previous={previous}, readback={readback}; \
                 the colour cannot be rolled back"
            );
        }
        Ok(())
    }
}

/// Verify a write and attempt at most one rollback on any failure after it starts.
/// The caller must validate both requested and previous values before calling.
fn guarded_write<T: Copy + Eq + std::fmt::Debug>(
    what: &str,
    previous: T,
    requested: T,
    mut write: impl FnMut(T) -> Result<()>,
    mut read: impl FnMut() -> Result<T>,
) -> Result<WriteOutcome<T>> {
    let failure = match write(requested) {
        Ok(()) => {
            thread::sleep(Duration::from_millis(20));
            match read() {
                Ok(readback) if readback == requested => {
                    return Ok(WriteOutcome {
                        previous,
                        requested,
                        readback,
                    });
                }
                Ok(readback) => format!("first readback={readback:?} (mismatch)"),
                Err(err) => format!("first readback unavailable: {err:#}"),
            }
        }
        // A missing or invalid acknowledgement does not prove the write was
        // ignored by the device, so still try to restore the previous value.
        Err(err) => format!("write failed: {err:#}; first readback not attempted"),
    };

    let rollback_write = write(previous);
    thread::sleep(Duration::from_millis(20));
    // Read even if rollback acknowledgement failed, to report the observed state.
    let rollback_readback = read();
    let rollback_succeeded = rollback_write.is_ok()
        && rollback_readback
            .as_ref()
            .is_ok_and(|value| *value == previous);
    let rollback_write = match rollback_write {
        Ok(()) => "acknowledged".to_string(),
        Err(err) => format!("failed: {err:#}"),
    };
    let rollback_readback = match rollback_readback {
        Ok(value) => format!("{value:?}"),
        Err(err) => format!("unavailable: {err:#}"),
    };

    bail!(
        "{what} write failed: previous={previous:?}, requested={requested:?}, {failure}; \
         rollback succeeded={rollback_succeeded}, rollback write={rollback_write}, \
         rollback readback={rollback_readback}"
    )
}

fn candidate_rank(candidate: &HidCandidate) -> (u8, i32, u16, u16) {
    let rank = if candidate.interface == 0
        && candidate.usage_page == 0x0001
        && candidate.usage == 0x0002
    {
        0
    } else if candidate.interface == 1 {
        1
    } else if candidate.interface == 3 {
        2
    } else {
        3
    };
    (
        rank,
        candidate.interface,
        candidate.usage_page,
        candidate.usage,
    )
}

fn require_success(response: &Response, what: &str) -> Result<()> {
    if response.is_success() {
        return Ok(());
    }

    bail!(
        "{what} request failed: {} (status 0x{:02X})",
        response.status_name(),
        response.status
    )
}

/// Check framing integrity and request echoes without interpreting device status.
/// Read sizes are hardware-confirmed; write response sizes remain unverified.
fn validate_response(
    response: &Response,
    transaction_id: u8,
    command_class: u8,
    command_id: u8,
    expected_size: u8,
) -> Result<()> {
    if usize::from(response.data_size) > MAX_ARGS {
        bail!(
            "response data size exceeds maximum: expected <= {MAX_ARGS}, actual {}",
            response.data_size
        );
    }
    if !response.crc_ok {
        bail!("response CRC validity mismatch: expected true, actual false");
    }
    if response.transaction_id != transaction_id {
        bail!(
            "response transaction ID mismatch: expected 0x{transaction_id:02X}, actual 0x{:02X}",
            response.transaction_id
        );
    }
    if response.class != command_class {
        bail!(
            "response class mismatch: expected 0x{command_class:02X}, actual 0x{:02X}",
            response.class
        );
    }
    if response.command != command_id {
        bail!(
            "response command mismatch: expected 0x{command_id:02X}, actual 0x{:02X}",
            response.command
        );
    }
    if command_id & 0x80 != 0 && response.data_size != expected_size {
        bail!(
            "response read data size mismatch: expected {expected_size}, actual {}",
            response.data_size
        );
    }
    Ok(())
}

fn send_and_read(
    dev: &HidDevice,
    transaction_id: u8,
    command_class: u8,
    command_id: u8,
    data_size: u8,
    args: &[u8],
) -> Result<Response> {
    let report = build_report(transaction_id, command_class, command_id, data_size, args);
    dev.send_feature_report(&report)
        .map_err(|err| anyhow!("HID feature write failed: {err}"))?;

    // Razer's firmware needs a moment to fill the response report.
    thread::sleep(Duration::from_millis(10));

    let mut response = [0u8; BUFFER_LEN];
    response[0] = REPORT_ID;
    let read = dev
        .get_feature_report(&mut response)
        .map_err(|err| anyhow!("HID feature read failed: {err}"))?;

    let parsed = parse_response(&response[..read])?;

    validate_response(
        &parsed,
        transaction_id,
        command_class,
        command_id,
        data_size,
    )?;

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_body(status: u8, tx: u8, class: u8, command: u8, args: &[u8]) -> Vec<u8> {
        let mut body = vec![0u8; BODY_LEN];
        body[0] = status;
        body[1] = tx;
        body[5] = args.len() as u8;
        body[6] = class;
        body[7] = command;
        body[8..8 + args.len()].copy_from_slice(args);
        body[88] = body[2..88].iter().fold(0, |acc, byte| acc ^ byte);
        body
    }

    #[test]
    fn report_layout_and_crc() {
        let report = build_report(0xFF, class::DPI, cmd::GET_DPI_STAGES, 38, &[VARSTORE]);

        assert_eq!(report[0], REPORT_ID);
        assert_eq!(report[2], 0xFF);
        assert_eq!(report[6], 38);
        assert_eq!(report[7], class::DPI);
        assert_eq!(report[8], cmd::GET_DPI_STAGES);
        assert_eq!(report[9], VARSTORE);
        assert_eq!(report[CRC_OFFSET], crc(&report));
        assert_eq!(
            report[CRC_OFFSET],
            report[3..=88].iter().fold(0, |acc, byte| acc ^ byte)
        );
    }

    #[test]
    fn rejects_oversized_response_framing() {
        assert!(
            parse_response(&[0; 92])
                .unwrap_err()
                .to_string()
                .contains("actual 92")
        );
    }

    #[test]
    fn rejects_nonzero_report_id() {
        let mut raw = [0; BUFFER_LEN];
        raw[0] = 0x01;
        let error = parse_response(&raw).unwrap_err().to_string();
        assert!(error.contains("expected 0x00, actual 0x01"));
    }

    #[test]
    fn rejects_declared_payload_above_capacity() {
        let mut body = response_body(STATUS_SUCCESS, 0xFF, 0, cmd::GET_FIRMWARE, &[1, 3]);
        body[5] = 81;
        assert!(
            parse_response(&body)
                .unwrap_err()
                .to_string()
                .contains("expected <= 80, actual 81")
        );
    }

    #[test]
    fn payload_excludes_padding() {
        let body = response_body(STATUS_SUCCESS, 0xFF, 0, cmd::GET_FIRMWARE, &[1, 3]);
        let response = parse_response(&body).unwrap();
        assert_eq!(response.args.len(), MAX_ARGS);
        assert_eq!(response.payload(), &[1, 3]);
        assert!(
            response
                .require_payload(22)
                .unwrap_err()
                .to_string()
                .contains("expected 22, actual 2")
        );
        let empty = response_body(STATUS_SUCCESS, 0xFF, 0, cmd::SET_POLL_RATE, &[]);
        assert!(parse_response(&empty).unwrap().payload().is_empty());
    }

    #[test]
    fn validates_read_response_and_leaves_status_to_caller() {
        for status in [STATUS_SUCCESS, STATUS_NOT_SUPPORTED] {
            let body = response_body(status, 0xFF, 0, cmd::GET_FIRMWARE, &[1, 3]);
            let response = parse_response(&body).unwrap();
            validate_response(&response, 0xFF, 0, cmd::GET_FIRMWARE, 2).unwrap();
            assert_eq!(
                require_success(&response, "firmware").is_ok(),
                status == STATUS_SUCCESS
            );
        }
    }

    #[test]
    fn validation_rejects_bad_crc_but_parser_reports_it() {
        let mut body = response_body(STATUS_SUCCESS, 0xFF, 0, cmd::GET_FIRMWARE, &[1, 3]);
        body[88] ^= 1;
        let response = parse_response(&body).unwrap();
        assert!(!response.crc_ok);
        let error = validate_response(&response, 0xFF, 0, cmd::GET_FIRMWARE, 2)
            .unwrap_err()
            .to_string();
        assert!(error.contains("CRC"));
        assert!(error.contains("expected true, actual false"));
    }

    #[test]
    fn validation_rejects_read_echo_mismatches() {
        let body = response_body(STATUS_SUCCESS, 0xFF, 0, cmd::GET_FIRMWARE, &[1, 3]);
        let response = parse_response(&body).unwrap();
        for (tx, class, command, size, expected_error) in [
            (
                0x3F,
                0,
                cmd::GET_FIRMWARE,
                2,
                "transaction ID mismatch: expected 0x3F, actual 0xFF",
            ),
            (
                0xFF,
                4,
                cmd::GET_FIRMWARE,
                2,
                "class mismatch: expected 0x04, actual 0x00",
            ),
            (
                0xFF,
                0,
                cmd::GET_SERIAL,
                2,
                "command mismatch: expected 0x82, actual 0x81",
            ),
            (
                0xFF,
                0,
                cmd::GET_FIRMWARE,
                22,
                "read data size mismatch: expected 22, actual 2",
            ),
        ] {
            let error = validate_response(&response, tx, class, command, size)
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected_error), "{error}");
        }
    }

    #[test]
    fn write_validation_allows_size_difference_but_checks_echoes() {
        let body = response_body(
            STATUS_SUCCESS,
            0x3F,
            class::MATRIX,
            cmd::SET_BRIGHTNESS,
            &[],
        );
        let response = parse_response(&body).unwrap();
        validate_response(&response, 0x3F, class::MATRIX, cmd::SET_BRIGHTNESS, 3).unwrap();
        for (class, command, expected_error) in [
            (
                class::STANDARD,
                cmd::SET_BRIGHTNESS,
                "class mismatch: expected 0x00, actual 0x0F",
            ),
            (
                class::MATRIX,
                cmd::SET_EFFECT,
                "command mismatch: expected 0x02, actual 0x04",
            ),
        ] {
            let error = validate_response(&response, 0x3F, class, command, 3)
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected_error), "{error}");
        }
    }

    #[test]
    fn validation_rejects_oversized_payload_for_reads_and_writes() {
        for command in [cmd::GET_POLL_RATE, cmd::SET_POLL_RATE] {
            let body = response_body(STATUS_SUCCESS, 0xFF, 0, command, &[1]);
            let mut response = parse_response(&body).unwrap();
            response.data_size = 81;
            let error = validate_response(&response, 0xFF, 0, command, 1)
                .unwrap_err()
                .to_string();
            assert!(error.contains("expected <= 80, actual 81"));
        }
    }

    #[test]
    fn dpi_stages_rejects_count_above_five() {
        let mut args = [0; 45];
        args[2] = 6;
        assert!(
            parse_dpi_stages(&args)
                .unwrap_err()
                .to_string()
                .contains("expected <= 5, actual 6")
        );
    }

    #[test]
    fn dpi_stages_rejects_missing_entries() {
        let mut args = [0; 10];
        args[2] = 2;
        assert!(
            parse_dpi_stages(&args)
                .unwrap_err()
                .to_string()
                .contains("expected at least 17 bytes for 2 stages, actual 10")
        );
        assert!(parse_dpi_stages(&[0, 1]).is_err());
    }

    #[test]
    fn parses_successful_response() {
        let body = response_body(
            STATUS_SUCCESS,
            0xFF,
            class::STANDARD,
            cmd::GET_POLL_RATE,
            &[0x08],
        );
        let response = parse_response(&body).expect("response should parse");

        assert!(response.is_success());
        assert!(response.crc_ok);
        assert_eq!(response.transaction_id, 0xFF);
        assert_eq!(response.class, class::STANDARD);
        assert_eq!(response.command, cmd::GET_POLL_RATE);
        assert_eq!(response.arg(0), Some(0x08));
    }

    #[test]
    fn parses_response_with_report_id() {
        let body = response_body(
            STATUS_SUCCESS,
            0x1F,
            class::STANDARD,
            cmd::GET_FIRMWARE,
            &[1, 2],
        );
        let mut buffer = vec![REPORT_ID];
        buffer.extend_from_slice(&body);

        let response = parse_response(&buffer).expect("response should parse");
        assert_eq!(response.transaction_id, 0x1F);
        assert_eq!(response.args[0], 1);
        assert_eq!(response.args[1], 2);
    }

    #[test]
    fn rejects_short_response() {
        assert!(parse_response(&[0u8; 10]).is_err());
    }

    #[test]
    fn maps_poll_rate_bytes() {
        assert_eq!(poll_rate_from_byte(0x01), Some(1000));
        assert_eq!(poll_rate_from_byte(0x02), Some(500));
        assert_eq!(poll_rate_from_byte(0x08), Some(125));
        assert_eq!(poll_rate_from_byte(0x10), None);
    }

    #[test]
    fn encodes_poll_rates_and_rejects_invalid_values() {
        for (hz, byte) in [(125, 0x08), (500, 0x02), (1000, 0x01)] {
            assert_eq!(poll_rate_to_byte(hz).unwrap(), byte);
            assert_eq!(poll_rate_from_byte(byte), Some(hz));
        }
        for hz in [0, 1, 124, 126, 250, 499, 501, 999, 1001, u16::MAX] {
            let err = poll_rate_to_byte(hz).unwrap_err().to_string();
            assert!(err.contains(&format!("{hz} Hz")));
            assert!(err.contains("125, 500, or 1000 Hz"));
        }
    }

    #[test]
    fn encodes_dpi_write_args() {
        assert_eq!(
            dpi_write_args(800, 8500).unwrap(),
            [0x01, 0x03, 0x20, 0x21, 0x34, 0x00, 0x00]
        );
    }

    #[test]
    fn validates_both_dpi_axes() {
        for (x, y) in [(100, 100), (100, 8500), (8500, 100), (8500, 8500)] {
            assert!(dpi_write_args(x, y).is_ok());
        }
        for invalid in [0, 1, 99, 8501, u16::MAX] {
            assert!(dpi_write_args(invalid, 800).is_err());
            assert!(dpi_write_args(800, invalid).is_err());
        }
    }

    #[test]
    fn encodes_brightness_write_args() {
        for brightness in [0, 128, 255] {
            assert_eq!(brightness_write_args(brightness), [0x01, 0x04, brightness]);
        }
    }

    #[test]
    fn encodes_static_colour_write_args() {
        assert_eq!(
            static_write_args(0x12, 0x34, 0x56),
            [0x01, 0x04, 0x01, 0x00, 0x00, 0x01, 0x12, 0x34, 0x56]
        );
        assert_eq!(
            static_write_args(0, 255, 0),
            [0x01, 0x04, 0x01, 0x00, 0x00, 0x01, 0x00, 0xFF, 0x00]
        );
    }

    #[test]
    fn dpi_write_report_layout_and_crc() {
        let args = dpi_write_args(800, 8500).unwrap();
        let report = build_report(0xFF, class::DPI, cmd::SET_DPI, 7, &args);

        assert_eq!(
            &report[..16],
            &[
                0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x07, 0x04, 0x05, 0x01, 0x03, 0x20, 0x21, 0x34,
                0x00, 0x00,
            ]
        );
        assert!(report[16..89].iter().all(|byte| *byte == 0));
        assert_eq!(report[89], 0x31);
        assert_eq!(report[90], 0);
        assert_eq!(crc(&report), 0x31);
    }

    #[test]
    fn guard_returns_verified_values() {
        let mut writes = Vec::new();
        let outcome = guarded_write(
            "DPI",
            (800, 800),
            (1600, 1200),
            |value| {
                writes.push(value);
                Ok(())
            },
            || Ok((1600, 1200)),
        )
        .unwrap();

        assert_eq!(writes, [(1600, 1200)]);
        assert_eq!(outcome.previous, (800, 800));
        assert_eq!(outcome.requested, (1600, 1200));
        assert_eq!(outcome.readback, (1600, 1200));
    }

    #[test]
    fn guard_writes_and_reads_even_when_value_is_unchanged() {
        let mut writes = Vec::new();
        let mut reads = 0;
        let outcome = guarded_write(
            "polling rate",
            1000,
            1000,
            |value| {
                writes.push(value);
                Ok(())
            },
            || {
                reads += 1;
                Ok(1000)
            },
        )
        .unwrap();

        assert_eq!(writes, [1000]);
        assert_eq!(reads, 1);
        assert_eq!(outcome.previous, 1000);
        assert_eq!(outcome.requested, 1000);
        assert_eq!(outcome.readback, 1000);
    }

    #[test]
    fn guard_rolls_back_mismatch_and_reports_observed_values() {
        let calls = std::cell::RefCell::new(Vec::new());
        let mut readbacks = [900, 800].into_iter();
        let err = guarded_write(
            "DPI",
            800,
            1000,
            |value| {
                calls.borrow_mut().push(format!("write {value}"));
                Ok(())
            },
            || {
                calls.borrow_mut().push("read".to_string());
                Ok(readbacks.next().expect("at most two reads"))
            },
        )
        .unwrap_err()
        .to_string();

        assert_eq!(
            calls.into_inner(),
            ["write 1000", "read", "write 800", "read"]
        );
        for detail in [
            "previous=800",
            "requested=1000",
            "first readback=900",
            "rollback succeeded=true",
            "rollback readback=800",
        ] {
            assert!(err.contains(detail), "missing {detail:?}: {err}");
        }
    }

    #[test]
    fn guard_stops_after_one_rollback_mismatch() {
        let mut writes = Vec::new();
        let mut readbacks = [900, 950].into_iter();
        let err = guarded_write(
            "DPI",
            800,
            1000,
            |value| {
                writes.push(value);
                Ok(())
            },
            || Ok(readbacks.next().expect("at most two reads")),
        )
        .unwrap_err()
        .to_string();

        assert_eq!(writes, [1000, 800]);
        assert!(err.contains("first readback=900"));
        assert!(err.contains("rollback succeeded=false"));
        assert!(err.contains("rollback readback=950"));
    }

    #[test]
    fn guard_rolls_back_when_first_readback_fails() {
        let mut writes = Vec::new();
        let mut readbacks = [Err(anyhow!("read disconnected")), Ok(800)].into_iter();
        let err = guarded_write(
            "DPI",
            800,
            1000,
            |value| {
                writes.push(value);
                Ok(())
            },
            || readbacks.next().expect("at most two reads"),
        )
        .unwrap_err()
        .to_string();

        assert_eq!(writes, [1000, 800]);
        assert!(err.contains("first readback unavailable: read disconnected"));
        assert!(err.contains("rollback succeeded=true"));
        assert!(err.contains("rollback readback=800"));
    }

    #[test]
    fn guard_rolls_back_when_write_acknowledgement_fails() {
        let mut writes = Vec::new();
        let mut reads = 0;
        let err = guarded_write(
            "DPI",
            800,
            1000,
            |value| {
                writes.push(value);
                if value == 1000 {
                    bail!("invalid acknowledgement");
                }
                Ok(())
            },
            || {
                reads += 1;
                Ok(800)
            },
        )
        .unwrap_err()
        .to_string();

        assert_eq!(writes, [1000, 800]);
        assert_eq!(reads, 1);
        assert!(err.contains("write failed: invalid acknowledgement"));
        assert!(err.contains("rollback succeeded=true"));
        assert!(err.contains("rollback readback=800"));
    }

    #[test]
    fn guard_reports_both_rollback_errors() {
        let mut writes = Vec::new();
        let mut readbacks = [Ok(900), Err(anyhow!("rollback read failed"))].into_iter();
        let err = guarded_write(
            "DPI",
            800,
            1000,
            |value| {
                writes.push(value);
                if value == 800 {
                    bail!("rollback acknowledgement failed");
                }
                Ok(())
            },
            || readbacks.next().expect("at most two reads"),
        )
        .unwrap_err()
        .to_string();

        assert_eq!(writes, [1000, 800]);
        assert!(err.contains("first readback=900"));
        assert!(err.contains("rollback succeeded=false"));
        assert!(err.contains("rollback write=failed: rollback acknowledgement failed"));
        assert!(err.contains("rollback readback=unavailable: rollback read failed"));
    }

    #[test]
    fn guard_requires_rollback_acknowledgement_even_when_previous_value_is_restored() {
        let mut writes = Vec::new();
        let mut readbacks = [900, 800].into_iter();
        let err = guarded_write(
            "DPI",
            800,
            1000,
            |value| {
                writes.push(value);
                if value == 800 {
                    bail!("rollback acknowledgement failed");
                }
                Ok(())
            },
            || Ok(readbacks.next().expect("at most two reads")),
        )
        .unwrap_err()
        .to_string();

        assert_eq!(writes, [1000, 800]);
        assert!(err.contains("rollback succeeded=false"));
        assert!(err.contains("rollback readback=800"));
    }

    #[test]
    fn parses_dpi_stages() {
        let mut args = vec![VARSTORE, 2, 3];
        for (stage, (x, y)) in [(400u16, 400u16), (800, 800), (1600, 1200)]
            .into_iter()
            .enumerate()
        {
            args.push(stage as u8);
            args.extend_from_slice(&x.to_be_bytes());
            args.extend_from_slice(&y.to_be_bytes());
            args.extend_from_slice(&[0, 0]);
        }

        let stages = parse_dpi_stages(&args).expect("stages should parse");
        assert_eq!(stages.active_stage, 2);
        assert_eq!(stages.count, 3);
        assert_eq!(stages.stages, vec![(400, 400), (800, 800), (1600, 1200)]);
    }
}
