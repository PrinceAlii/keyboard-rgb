//! Detect other programs that write to the same keyboard.
//!
//! The AULA F99 accepts `0x08` colour frames from any process that opens its
//! vendor HID collection. There is no cross-process lock in the firmware, so two
//! lighting programs alternate frames and the board appears to flicker. The
//! best a user-space driver can do is notice the usual suspects and tell the
//! user to close them.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

/// How often the background thread rescans the process table.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A known program that can drive the same HID collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// Human-readable program name.
    pub label: &'static str,
    /// Executable names that were actually found.
    pub processes: Vec<&'static str>,
}

struct Known {
    label: &'static str,
    process: &'static str,
}

const KNOWN: &[Known] = &[
    Known {
        label: "SignalRGB",
        process: "SignalRgb.exe",
    },
    Known {
        label: "SignalRGB",
        process: "SignalRgbLauncher.exe",
    },
    Known {
        label: "OpenRGB",
        process: "OpenRGB.exe",
    },
    Known {
        label: "AULA F99 software",
        process: "OemDrv.exe",
    },
    Known {
        label: "another rgbd instance",
        process: "rgbd.exe",
    },
    Known {
        label: "rgbctl",
        process: "rgbctl.exe",
    },
];

/// Watches the process table and publishes the current conflict list.
pub struct ConflictMonitor {
    rx: Receiver<Vec<Conflict>>,
}

impl ConflictMonitor {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        let _ = thread::Builder::new()
            .name("rgbd-conflicts".to_string())
            .spawn(move || {
                loop {
                    let _ = tx.send(detect());
                    thread::sleep(POLL_INTERVAL);
                }
            });
        Self { rx }
    }

    /// Latest scan result, or `None` if the background thread has not reported
    /// yet. Always drains older reports so a window restored after being
    /// minimized cannot replay a stale conflict list. Never blocks the UI.
    pub fn poll(&self) -> Option<Vec<Conflict>> {
        self.rx.try_iter().last()
    }
}

/// Run one scan synchronously. Used by the CLI for a startup warning.
pub fn active_conflicts() -> Vec<Conflict> {
    detect()
}

fn detect() -> Vec<Conflict> {
    let names = process_names();
    let self_name = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default();

    let mut conflicts: Vec<Conflict> = Vec::new();

    for known in KNOWN {
        let mut count = names
            .iter()
            .filter(|name| name.eq_ignore_ascii_case(known.process))
            .count();

        // `rgbd.exe` is this process; don't report ourselves. A second
        // `rgbd.exe` or any `rgbctl.exe` is a real second writer.
        if self_name.eq_ignore_ascii_case(known.process) {
            count = count.saturating_sub(1);
        }
        if count == 0 {
            continue;
        }

        if let Some(existing) = conflicts
            .iter_mut()
            .find(|conflict| conflict.label == known.label)
        {
            existing.processes.push(known.process);
        } else {
            conflicts.push(Conflict {
                label: known.label,
                processes: vec![known.process],
            });
        }
    }

    conflicts
}

#[cfg(windows)]
fn process_names() -> Vec<String> {
    use std::os::windows::process::CommandExt;

    // Hide the transient console window when the GUI launches `tasklist`.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let output = std::process::Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(csv_first_field)
        .collect()
}

#[cfg(not(windows))]
fn process_names() -> Vec<String> {
    Vec::new()
}

/// Pull the image name out of `"SignalRgb.exe","36668","Console",...`.
fn csv_first_field(line: &str) -> Option<String> {
    let rest = line.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tasklist_csv_image_name() {
        let line = r#""SignalRgb.exe","36668","Console","1","123,456 K""#;
        assert_eq!(csv_first_field(line).as_deref(), Some("SignalRgb.exe"));
    }

    #[test]
    fn ignores_tasklist_noise() {
        assert_eq!(csv_first_field("INFO: No tasks are running"), None);
        assert_eq!(csv_first_field(""), None);
    }
}
