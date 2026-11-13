//! SinoWealth `258A:010C` lighting protocol used by the AULA F99.
//!
//! The device speaks 520-byte HID **feature** reports, report ID `0x06`:
//!
//! ```text
//! byte 0      report ID, always 0x06
//! byte 1      command; bit 7 set = read, clear = write
//! bytes 2..5  address (00 00 01 00 for colour and config)
//! bytes 6..7  payload length, little-endian
//! bytes 8..   payload, zero padded to 512 bytes
//! ```
//!
//! There are three colour paths, and they are not interchangeable:
//!
//! * `0x06` — static per-key, planar (128 R, 128 G, 128 B), persists.
//!   Repeated writes can blank the board, so this driver never uses it.
//! * `0x08` — live streaming, interleaved RGB, 126 slots. This is the animation
//!   path and the only one this driver writes.
//! * `0x0A` — solid single colour, 512-byte payload.
//!
//! Config reads (`0x84`) are read-only and safe. Config writes (`0x04`) are
//! non-volatile. The only write this driver offers is a guarded
//! read-modify-write of the three lighting-mode bytes; it validates the block,
//! waits out the firmware's asynchronous repaint, and verifies the readback.

use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use hidapi::HidDevice;

use crate::color::Rgb;
use crate::hid::{HidCandidate, HidManager};

/// The report ID every RGB command uses.
pub const REPORT_ID: u8 = 0x06;

pub const HEADER_LEN: usize = 8;
pub const PAYLOAD_LEN: usize = 512;
pub const PACKET_LEN: usize = HEADER_LEN + PAYLOAD_LEN;

/// Colour and config share this address. The command selects the region; the
/// address only distinguishes the real framebuffer from the staging buffer at
/// `00 00 00 00`, which accepts writes and drives nothing.
pub const ADDR: [u8; 4] = [0x00, 0x00, 0x01, 0x00];

pub const CONFIG_LEN: usize = 0x0080;
pub const CONFIG_MAGIC: [u8; 2] = [0x5A, 0xA5];

/// The three config bytes that select the lighting mode.
pub const MODE_OFFSETS: [usize; 3] = [9, 10, 59];

/// Host-streamed per-key colours only render in this mode.
pub const MODE_PER_KEY: [u8; 3] = [0x01, 0x15, 0x47];

/// The firmware's own solid mode, for reference. Not written by this driver.
pub const MODE_SOLID: [u8; 3] = [0x00, 0x01, 0x40];

/// A config write triggers an asynchronous repaint inside the firmware that
/// lands hundreds of milliseconds later and overwrites anything streamed in
/// the meantime. Wait this long before sending the first frame.
pub const CONFIG_SETTLE_MS: u64 = 500;

pub const STREAM_LEN: usize = 0x017A; // 378 bytes
pub const STREAM_SLOTS: usize = STREAM_LEN / 3; // 126

/// LED-map table shape returned by command `0x83`.
pub const LED_MAP_ENTRIES: usize = 128;
pub const LED_MAP_ENTRY_SIZE: usize = 4;

/// The wired firmware's measured floor is ~44.7 ms between colour writes.
/// 50 ms (20 FPS) leaves headroom and avoids starving the key-scan loop, which
/// shows up as dropped or doubled keypresses rather than as a lighting fault.
pub const MIN_FRAME_GAP_MS: u64 = 50;
pub const MAX_FPS: u32 = 20;

/// Resend an unchanged frame this often so the board recovers if the firmware
/// repaints over us. Normal animation already sends every frame.
pub const KEEPALIVE_MS: u64 = 1_000;

/// OpenRGB's model ID for the AULA F99.
pub const AULA_F99_MODEL_ID: u8 = 0xA4;

pub mod cmd {
    /// Write the 128-byte config block. Non-volatile; not implemented yet.
    pub const WRITE_CONFIG: u8 = 0x04;
    /// Read the 128-byte config block.
    pub const READ_CONFIG: u8 = 0x84;

    /// Static per-key colours, planar, persists. Never used by this driver.
    pub const WRITE_STATIC: u8 = 0x06;
    /// Live streaming colours, interleaved.
    pub const STREAM: u8 = 0x08;
    /// Solid single colour.
    pub const WRITE_SOLID: u8 = 0x0A;

    /// Model / firmware query. Model ID is response byte 13.
    pub const QUERY_MODEL: u8 = 0x82;
    /// Key matrix table: 4 bytes per entry, HID usage code in byte 3.
    pub const READ_MATRIX: u8 = 0x83;
}

/// Build a 520-byte feature-report packet.
pub fn build_packet(
    command: u8,
    address: [u8; 4],
    payload_len: u16,
    payload: Option<&[u8]>,
) -> [u8; PACKET_LEN] {
    let mut packet = [0u8; PACKET_LEN];
    packet[0] = REPORT_ID;
    packet[1] = command;
    packet[2..6].copy_from_slice(&address);
    packet[6..8].copy_from_slice(&payload_len.to_le_bytes());

    if let Some(payload) = payload {
        let len = payload.len().min(PAYLOAD_LEN);
        packet[HEADER_LEN..HEADER_LEN + len].copy_from_slice(&payload[..len]);
    }

    packet
}

/// The exact model-query packet OpenRGB's `SinowealthKeyboard10c` detector uses.
pub fn model_query_packet() -> [u8; PACKET_LEN] {
    build_packet(cmd::QUERY_MODEL, [0x01, 0x00, 0x01, 0x00], 0x0006, None)
}

/// Build the interleaved RGB payload for the streaming path.
pub fn stream_packet(colors: &[Rgb]) -> [u8; PACKET_LEN] {
    let mut payload = [0u8; STREAM_LEN];
    for (slot, color) in colors.iter().take(STREAM_SLOTS).enumerate() {
        payload[slot * 3] = color.r;
        payload[slot * 3 + 1] = color.g;
        payload[slot * 3 + 2] = color.b;
    }
    build_packet(cmd::STREAM, ADDR, STREAM_LEN as u16, Some(&payload))
}

/// Does this block look like a real config read?
pub fn is_valid_config(config: &[u8]) -> bool {
    config.len() == CONFIG_LEN
        && config[CONFIG_LEN - 2] == CONFIG_MAGIC[0]
        && config[CONFIG_LEN - 1] == CONFIG_MAGIC[1]
}

/// The three bytes that select the lighting mode.
pub fn mode_of(config: &[u8; CONFIG_LEN]) -> [u8; 3] {
    MODE_OFFSETS.map(|offset| config[offset])
}

/// Is this config in host-streamed per-key mode?
pub fn is_per_key_mode(config: &[u8; CONFIG_LEN]) -> bool {
    mode_of(config) == MODE_PER_KEY
}

/// Copy a config block and replace only the three mode bytes.
///
/// This is a read-modify-write of the keyboard's *own* block, never a block
/// captured from another device or link. The whole block is validated before
/// it is allowed to become a write.
pub fn with_mode(config: &[u8; CONFIG_LEN], mode: [u8; 3]) -> Result<[u8; CONFIG_LEN]> {
    if !is_valid_config(config) {
        bail!("refusing to patch a config block without the 5A A5 signature");
    }
    let mut patched = *config;
    for (offset, value) in MODE_OFFSETS.into_iter().zip(mode) {
        patched[offset] = value;
    }
    Ok(patched)
}

/// Byte offsets that differ between two config blocks, with their values.
pub fn config_changes(before: &[u8; CONFIG_LEN], after: &[u8; CONFIG_LEN]) -> Vec<(usize, u8, u8)> {
    before
        .iter()
        .zip(after)
        .enumerate()
        .filter(|(_, (old, new))| old != new)
        .map(|(index, (old, new))| (index, *old, *new))
        .collect()
}

/// One 4-byte entry from the keyboard's LED map.
///
/// The vendor table packs three different kinds of key into one shape:
///
/// ```text
/// byte 0      special-key code (0x0D = Fn)
/// byte 1      modifier mask (0x01 LCtrl, 0x02 LShift, 0x04 LAlt, 0x08 LWin,
///             0x10 RCtrl, 0x20 RShift, 0x40 RAlt)
/// bytes 2..3  HID usage code, big-endian, for normal keys
/// ```
///
/// The final entry is the footer `00 00 5A A5`, not a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedMapEntry {
    pub special: u8,
    pub modifier: u8,
    pub usage: u16,
}

impl LedMapEntry {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < LED_MAP_ENTRY_SIZE {
            return None;
        }
        Some(Self {
            special: bytes[0],
            modifier: bytes[1],
            usage: u16::from_be_bytes([bytes[2], bytes[3]]),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.special == 0 && self.modifier == 0 && self.usage == 0
    }

    pub fn is_footer(&self) -> bool {
        self.special == 0 && self.modifier == 0 && self.usage == 0x5AA5
    }

    /// Human-readable key name for diagnostics.
    pub fn label(&self) -> String {
        if self.is_footer() {
            return "footer (5A A5)".to_string();
        }
        if self.modifier != 0 {
            return modifier_label(self.modifier);
        }
        if self.usage != 0 {
            return usage_label(self.usage);
        }
        match self.special {
            0x0D => "Fn".to_string(),
            0 => "empty".to_string(),
            code => format!("special 0x{code:02X}"),
        }
    }
}

/// Parse the 512-byte payload returned by command `0x83`.
pub fn parse_led_map(payload: &[u8]) -> Vec<LedMapEntry> {
    let (entries, _) = payload.as_chunks::<LED_MAP_ENTRY_SIZE>();
    entries
        .iter()
        .take(LED_MAP_ENTRIES)
        .filter_map(|entry| LedMapEntry::parse(entry))
        .collect()
}

pub fn modifier_label(mask: u8) -> String {
    let mut names = Vec::new();
    for (bit, name) in [
        (0x01, "LCtrl"),
        (0x02, "LShift"),
        (0x04, "LAlt"),
        (0x08, "LWin"),
        (0x10, "RCtrl"),
        (0x20, "RShift"),
        (0x40, "RAlt"),
    ] {
        if mask & bit != 0 {
            names.push(name);
        }
    }
    if names.is_empty() {
        format!("modifier 0x{mask:02X}")
    } else {
        names.join("+")
    }
}

/// Name a key from the USB HID keyboard usage page.
pub fn usage_label(usage: u16) -> String {
    const LETTERS: [&str; 26] = [
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
        "S", "T", "U", "V", "W", "X", "Y", "Z",
    ];
    const DIGITS: [&str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];
    const FUNCTION_KEYS: [&str; 12] = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    ];

    if (0x04..=0x1D).contains(&usage) {
        return LETTERS[(usage - 0x04) as usize].to_string();
    }
    if (0x1E..=0x27).contains(&usage) {
        return DIGITS[(usage - 0x1E) as usize].to_string();
    }
    if (0x3A..=0x45).contains(&usage) {
        return FUNCTION_KEYS[(usage - 0x3A) as usize].to_string();
    }

    let name = match usage {
        0x28 => "Enter",
        0x29 => "Esc",
        0x2A => "Backspace",
        0x2B => "Tab",
        0x2C => "Space",
        0x2D => "-",
        0x2E => "=",
        0x2F => "[",
        0x30 => "]",
        0x31 => "\\",
        0x32 => "NonUS#",
        0x33 => ";",
        0x34 => "'",
        0x35 => "`",
        0x36 => ",",
        0x37 => ".",
        0x38 => "/",
        0x39 => "Caps Lock",
        0x46 => "Print Screen",
        0x47 => "Scroll Lock",
        0x48 => "Pause",
        0x49 => "Insert",
        0x4A => "Home",
        0x4B => "Page Up",
        0x4C => "Delete",
        0x4D => "End",
        0x4E => "Page Down",
        0x4F => "Right",
        0x50 => "Left",
        0x51 => "Down",
        0x52 => "Up",
        0x53 => "Num Lock",
        0x54 => "KP /",
        0x55 => "KP *",
        0x56 => "KP -",
        0x57 => "KP +",
        0x58 => "KP Enter",
        0x59..=0x61 => {
            return format!("KP {}", usage - 0x58);
        }
        0x62 => "KP 0",
        0x63 => "KP .",
        0x64 => "NonUS \\",
        0x65 => "Application",
        0x66 => "Power",
        0x67 => "KP =",
        0x68..=0x73 => {
            return format!("F{}", usage - 0x5B);
        }
        0xE0 => "LCtrl",
        0xE1 => "LShift",
        0xE2 => "LAlt",
        0xE3 => "LWin",
        0xE4 => "RCtrl",
        0xE5 => "RShift",
        0xE6 => "RAlt",
        0xE7 => "RWin",
        _ => return format!("usage 0x{usage:04X}"),
    };
    name.to_string()
}

pub fn model_name(model_id: u8) -> &'static str {
    match model_id {
        AULA_F99_MODEL_ID => "AULA F99",
        _ => "unknown Sinowealth keyboard",
    }
}

/// One open keyboard and the read-only identity data gathered at open time.
pub struct Keyboard {
    dev: HidDevice,
    pub path: String,
    pub product: String,
    pub model_id: Option<u8>,
    /// `None` when the firmware was too busy to return a valid block at open
    /// time. Streaming does not need it; config backups and mode writes do.
    pub config: Option<[u8; CONFIG_LEN]>,
}

impl Keyboard {
    /// Find the first AULA vendor collection that answers the RGB protocol.
    ///
    /// The Windows keyboard and mouse top-level collections are opened
    /// exclusively by the OS. The three vendor collections all report usage
    /// page `0xFF00`, so the only reliable selector is to ask each one for its
    /// config block and require the `5A A5` signature.
    pub fn open(manager: &HidManager) -> Result<Self> {
        let candidates = manager.aula_vendor_collections();
        if candidates.is_empty() {
            bail!(
                "no AULA F99 vendor HID collections found (expected {:04X}:{:04X} usage page 0xFF00)",
                crate::hid::AULA_VID,
                crate::hid::AULA_PID
            );
        }

        let mut failures = Vec::new();
        for candidate in &candidates {
            match Self::open_candidate(manager, candidate) {
                Ok(keyboard) => return Ok(keyboard),
                Err(err) => failures.push(format!("{}: {err:#}", candidate.path_display)),
            }
        }

        bail!(
            "no AULA F99 vendor collection answered the RGB protocol:\n  {}",
            failures.join("\n  ")
        )
    }

    /// Open one specific collection and confirm what it is.
    ///
    /// A valid config block is the strongest test. If the firmware is too busy
    /// to return one, the model ID `0xA4` is accepted instead: it is read-only
    /// and cannot be produced by the wrong vendor collection.
    pub fn open_candidate(manager: &HidManager, candidate: &HidCandidate) -> Result<Self> {
        let dev = manager.open(candidate)?;
        let mut keyboard = Self {
            dev,
            path: candidate.path_display.clone(),
            product: candidate.product.clone(),
            model_id: None,
            config: None,
        };

        let config = keyboard.read_config_verified();
        let model_id = keyboard.query_model();

        match (config, model_id) {
            (Ok(config), model_id) => {
                keyboard.config = Some(config);
                keyboard.model_id = model_id.ok();
                Ok(keyboard)
            }
            (Err(_config_error), Ok(AULA_F99_MODEL_ID)) => {
                keyboard.config = None;
                keyboard.model_id = Some(AULA_F99_MODEL_ID);
                Ok(keyboard)
            }
            (Err(config_error), Ok(other)) => {
                bail!(
                    "collection accepted feature reports but is not a known F99 \
                     (model ID 0x{other:02X}; config: {config_error:#})"
                )
            }
            (Err(config_error), Err(model_error)) => {
                bail!(
                    "collection accepted feature reports but did not identify as an F99 \
                     (config: {config_error:#}; model: {model_error:#})"
                )
            }
        }
    }

    /// Send one 520-byte feature report and return the response.
    fn request(&self, packet: &[u8; PACKET_LEN]) -> Result<Vec<u8>> {
        self.dev
            .send_feature_report(packet)
            .map_err(|err| anyhow!("HID feature write failed: {err}"))?;

        let mut response = [0u8; PACKET_LEN];
        response[0] = REPORT_ID;
        let read = self
            .dev
            .get_feature_report(&mut response)
            .map_err(|err| anyhow!("HID feature read failed: {err}"))?;
        Ok(response[..read].to_vec())
    }

    /// Query the model ID (`0x82`). The F99 answers `0xA4`.
    ///
    /// The first read after a config read can return a stale/short buffer, so
    /// require the echoed command header and retry a few times.
    pub fn query_model(&self) -> Result<u8> {
        for attempt in 0..4 {
            if attempt > 0 {
                thread::sleep(Duration::from_millis(20));
            }
            let response = self.query_model_raw()?;
            if response.len() > 13 && response[0] == REPORT_ID && response[1] == cmd::QUERY_MODEL {
                return Ok(response[13]);
            }
        }
        bail!("model query returned no recognizable response")
    }

    /// Raw model-query response, for diagnostics.
    pub fn query_model_raw(&self) -> Result<Vec<u8>> {
        self.request(&model_query_packet())
    }

    fn read_config_once(&self) -> Result<[u8; CONFIG_LEN]> {
        let packet = build_packet(cmd::READ_CONFIG, ADDR, CONFIG_LEN as u16, None);
        let response = self.request(&packet)?;

        if response.len() < HEADER_LEN + CONFIG_LEN {
            bail!("config response too short ({} bytes)", response.len());
        }

        let mut config = [0u8; CONFIG_LEN];
        config.copy_from_slice(&response[HEADER_LEN..HEADER_LEN + CONFIG_LEN]);
        if !is_valid_config(&config) {
            bail!(
                "config signature missing (got {:02X} {:02X} at the end)",
                config[CONFIG_LEN - 2],
                config[CONFIG_LEN - 1]
            );
        }
        Ok(config)
    }

    /// Read until one valid block is returned.
    ///
    /// This is enough to identify the device and to display its mode. It is
    /// deliberately weaker than [`Self::read_config_stable`]: a config write
    /// needs two identical reads, but opening the device only needs proof that
    /// this collection speaks the protocol.
    pub fn read_config_verified(&self) -> Result<[u8; CONFIG_LEN]> {
        let mut last_error = None;
        for attempt in 0..8 {
            if attempt > 0 {
                thread::sleep(Duration::from_millis(25));
            }
            match self.read_config_once() {
                Ok(config) => return Ok(config),
                Err(err) => last_error = Some(err),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("config read returned no valid block")))
    }

    /// Read the config until two consecutive reads are identical and valid.
    ///
    /// The firmware returns truncated, mostly-zero buffers while busy. A single
    /// bad read accepted into a read-modify-write would zero the config block,
    /// so this refuses to return one.
    pub fn read_config_stable(&self) -> Result<[u8; CONFIG_LEN]> {
        let mut previous: Option<[u8; CONFIG_LEN]> = None;
        let mut last_error = None;

        for attempt in 0..10 {
            if attempt > 0 {
                thread::sleep(Duration::from_millis(30));
            }
            match self.read_config_once() {
                Ok(config) => {
                    if previous == Some(config) {
                        return Ok(config);
                    }
                    previous = Some(config);
                }
                Err(err) => last_error = Some(err),
            }
        }

        match last_error {
            Some(err) => Err(err.context("config read did not stabilise")),
            None => bail!("config read returned different values every time; refusing to trust it"),
        }
    }

    /// Write a complete, validated 128-byte config block (`0x04`).
    ///
    /// This is the only non-volatile operation in the driver. Callers must
    /// already have read the block from *this* keyboard, validated it, and
    /// changed only the bytes they understand. The method waits out the
    /// firmware's asynchronous repaint before returning.
    pub fn write_config(&self, config: &[u8; CONFIG_LEN]) -> Result<()> {
        if !is_valid_config(config) {
            bail!("refusing to write a config block without the 5A A5 signature");
        }

        let packet = build_packet(cmd::WRITE_CONFIG, ADDR, CONFIG_LEN as u16, Some(config));
        self.dev
            .send_feature_report(&packet)
            .map_err(|err| anyhow!("HID config write failed: {err}"))?;

        thread::sleep(Duration::from_millis(CONFIG_SETTLE_MS));
        Ok(())
    }

    /// Read the config back and require it to match exactly.
    ///
    /// `read_config_stable` already refuses a single glitched read; this adds
    /// the comparison that catches a write that did not land.
    pub fn verify_config(&self, expected: &[u8; CONFIG_LEN]) -> Result<[u8; CONFIG_LEN]> {
        let actual = self.read_config_stable()?;
        if actual != *expected {
            let changes = config_changes(expected, &actual);
            let summary = changes
                .iter()
                .take(12)
                .map(|(index, want, got)| format!("[{index}] {want:02X} -> {got:02X}"))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "config readback does not match the block just written ({} byte(s) differ: {})",
                changes.len(),
                summary
            );
        }
        Ok(actual)
    }

    /// Read the key matrix / LED map (`0x83`).
    pub fn read_led_map(&self) -> Result<Vec<u8>> {
        let packet = build_packet(
            cmd::READ_MATRIX,
            [0x00, 0x00, 0x00, 0x00],
            PAYLOAD_LEN as u16,
            None,
        );
        let response = self.request(&packet)?;
        if response.len() <= HEADER_LEN {
            bail!("LED map response too short ({} bytes)", response.len());
        }
        Ok(response[HEADER_LEN..].to_vec())
    }

    /// The three bytes that select the lighting mode, if the config was read.
    pub fn mode_bytes(&self) -> Option<[u8; 3]> {
        self.config.map(|config| mode_of(&config))
    }

    /// Whether the config matches the documented F99/F75 per-key values.
    pub fn per_key_mode_hint(&self) -> Option<bool> {
        self.config.map(|config| is_per_key_mode(&config))
    }

    /// Stream one interleaved RGB frame (`0x08`).
    ///
    /// This is volatile and safe to repeat. It is the only colour command this
    /// driver writes; the static `0x06` path is deliberately absent.
    pub fn send_stream(&self, colors: &[Rgb]) -> Result<()> {
        let packet = stream_packet(colors);
        self.dev
            .send_feature_report(&packet)
            .map_err(|err| anyhow!("HID frame write failed: {err}"))
    }

    /// Send a pre-built packet. Used by the CLI to reuse a constant frame.
    pub fn send_packet(&self, packet: &[u8; PACKET_LEN]) -> Result<()> {
        self.dev
            .send_feature_report(packet)
            .map_err(|err| anyhow!("HID frame write failed: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_header_matches_the_documented_layout() {
        let packet = build_packet(cmd::STREAM, ADDR, STREAM_LEN as u16, None);

        assert_eq!(packet.len(), PACKET_LEN);
        assert_eq!(packet[0], REPORT_ID);
        assert_eq!(packet[1], 0x08);
        assert_eq!(&packet[2..6], &[0x00, 0x00, 0x01, 0x00]);
        assert_eq!(packet[6], 0x7A);
        assert_eq!(packet[7], 0x01);
    }

    #[test]
    fn model_query_matches_the_reference_detector() {
        let packet = model_query_packet();

        assert_eq!(
            &packet[..8],
            &[0x06, 0x82, 0x01, 0x00, 0x01, 0x00, 0x06, 0x00]
        );
    }

    #[test]
    fn stream_layout_is_interleaved() {
        let mut colors = vec![Rgb::BLACK; STREAM_SLOTS];
        colors[0] = Rgb::new(255, 0, 0);
        colors[1] = Rgb::new(0, 255, 0);
        let packet = stream_packet(&colors);

        assert_eq!(&packet[HEADER_LEN..HEADER_LEN + 6], &[255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn stream_packet_is_378_bytes_of_payload() {
        let colors = vec![Rgb::WHITE; STREAM_SLOTS];
        let packet = stream_packet(&colors);
        let length = u16::from_le_bytes([packet[6], packet[7]]);

        assert_eq!(length, 378);
        assert_eq!(packet[HEADER_LEN + 377], 255);
    }

    #[test]
    fn a_zeroed_config_is_rejected() {
        assert!(!is_valid_config(&[0u8; CONFIG_LEN]));
    }

    #[test]
    fn the_signature_makes_a_config_valid() {
        let mut config = [0u8; CONFIG_LEN];
        config[CONFIG_LEN - 2..].copy_from_slice(&CONFIG_MAGIC);
        assert!(is_valid_config(&config));
    }

    #[test]
    fn mode_patch_changes_only_the_three_mode_bytes() {
        let mut config = [0x11u8; CONFIG_LEN];
        config[CONFIG_LEN - 2..].copy_from_slice(&CONFIG_MAGIC);
        let patched = with_mode(&config, MODE_PER_KEY).unwrap();

        assert!(is_per_key_mode(&patched));
        assert_eq!(mode_of(&patched), MODE_PER_KEY);
        assert_eq!(
            config_changes(&config, &patched),
            vec![(9, 0x11, 0x01), (10, 0x11, 0x15), (59, 0x11, 0x47)]
        );
    }

    #[test]
    fn mode_patch_refuses_an_invalid_block() {
        let config = [0u8; CONFIG_LEN];
        assert!(with_mode(&config, MODE_PER_KEY).is_err());
    }

    #[test]
    fn f99_model_id_has_a_name() {
        assert_eq!(model_name(AULA_F99_MODEL_ID), "AULA F99");
    }

    #[test]
    fn led_map_parses_normal_keys_modifiers_and_fn() {
        let esc = LedMapEntry::parse(&[0x00, 0x00, 0x00, 0x29]).unwrap();
        assert_eq!(esc.usage, 0x29);
        assert_eq!(esc.label(), "Esc");

        let lshift = LedMapEntry::parse(&[0x00, 0x02, 0x00, 0x00]).unwrap();
        assert_eq!(lshift.label(), "LShift");

        let fn_key = LedMapEntry::parse(&[0x0D, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(fn_key.label(), "Fn");

        let footer = LedMapEntry::parse(&[0x00, 0x00, 0x5A, 0xA5]).unwrap();
        assert!(footer.is_footer());
    }

    #[test]
    fn led_map_names_common_usages() {
        assert_eq!(usage_label(0x04), "A");
        assert_eq!(usage_label(0x1D), "Z");
        assert_eq!(usage_label(0x1E), "1");
        assert_eq!(usage_label(0x27), "0");
        assert_eq!(usage_label(0x3A), "F1");
        assert_eq!(usage_label(0x45), "F12");
        assert_eq!(usage_label(0x4E), "Page Down");
        assert_eq!(usage_label(0x58), "KP Enter");
        assert_eq!(usage_label(0x62), "KP 0");
    }
}
