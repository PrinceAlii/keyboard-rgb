use std::ffi::{CStr, CString};

use anyhow::{Context, Result};
use hidapi::{DeviceInfo, HidApi, HidDevice};

/// AULA / SinoWealth wired keyboard.
pub const AULA_VID: u16 = 0x258A;
pub const AULA_PID: u16 = 0x010C;

/// Razer Viper Mini, wired.
pub const RAZER_VID: u16 = 0x1532;
pub const VIPER_MINI_PID: u16 = 0x008A;

/// The vendor collection that carries the RGB protocol.
pub const VENDOR_USAGE_PAGE: u16 = 0xFF00;

/// Every RGB command in this protocol uses report ID 6 and a 520-byte report.
pub const FEATURE_REPORT_ID: u8 = 0x06;
pub const FEATURE_REPORT_SIZE: usize = 520;

#[derive(Clone, Debug)]
pub struct HidCandidate {
    pub path: CString,
    pub path_display: String,
    pub product: String,
    pub manufacturer: String,
    pub usage_page: u16,
    pub usage: u16,
    pub interface: i32,
}

impl HidCandidate {
    fn from_info(info: &DeviceInfo) -> Self {
        Self {
            path: info.path().to_owned(),
            path_display: info.path().to_string_lossy().into_owned(),
            product: info.product_string().unwrap_or("").to_string(),
            manufacturer: info.manufacturer_string().unwrap_or("").to_string(),
            usage_page: info.usage_page(),
            usage: info.usage(),
            interface: info.interface_number(),
        }
    }
}

/// Owns the hidapi context and turns its borrowed device list into owned values.
pub struct HidManager {
    api: HidApi,
}

impl HidManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            api: HidApi::new().context("failed to initialise hidapi")?,
        })
    }

    /// All HID collections exposed by the AULA keyboard, including the
    /// keyboard/mouse collections Windows may refuse to share.
    pub fn aula_collections(&self) -> Vec<HidCandidate> {
        self.api
            .device_list()
            .filter(|info| info.vendor_id() == AULA_VID && info.product_id() == AULA_PID)
            .map(HidCandidate::from_info)
            .collect()
    }

    /// Vendor-page collections are the only ones we would ever send RGB
    /// commands to. Do not assume there is exactly one: the F99 exposes
    /// several collections on usage page 0xFF00, and Windows reports the same
    /// page for all of them. The protocol probe decides which one is real.
    pub fn aula_vendor_collections(&self) -> Vec<HidCandidate> {
        self.aula_collections()
            .into_iter()
            .filter(|candidate| candidate.usage_page == VENDOR_USAGE_PAGE)
            .collect()
    }

    pub fn open(&self, candidate: &HidCandidate) -> Result<HidDevice> {
        self.api
            .open_path(candidate.path.as_c_str())
            .with_context(|| format!("failed to open HID collection {}", candidate.path_display))
    }

    /// All HID collections exposed by the Razer Viper Mini.
    pub fn razer_collections(&self) -> Vec<HidCandidate> {
        self.api
            .device_list()
            .filter(|info| info.vendor_id() == RAZER_VID && info.product_id() == VIPER_MINI_PID)
            .map(HidCandidate::from_info)
            .collect()
    }
}

/// Parse a HID path back into a CStr when we only have the owned path string.
/// Kept for diagnostics/tests.
pub fn path_to_cstr(path: &str) -> Result<CString> {
    CString::new(path).context("HID path contained an interior NUL")
}

pub fn usage_page_name(page: u16) -> &'static str {
    match page {
        0x0001 => "generic desktop",
        0x000C => "consumer",
        0xFF00 => "vendor",
        _ => "other",
    }
}

pub fn cstr_to_string(value: &CStr) -> String {
    value.to_string_lossy().into_owned()
}
