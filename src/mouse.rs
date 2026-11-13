//! Background Razer Viper Mini controller.
//!
//! One worker holds the device open and owns all HID access; the GUI only sends
//! commands and polls events. Polling, DPI, and brightness use the protocol's
//! guarded writes, readback verification, and single rollback attempt. DPI and
//! brightness ask for onboard storage; unplug/replug persistence is untested.
//! Static colour cannot be verified by
//! readback or rolled back, and requires explicit acceptance on each command.

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::color::Rgb;
use crate::hid::HidManager;
use crate::razer::{self, Device as RazerDevice, DpiStages, WriteOutcome};

/// A complete read of the connected mouse, including its actual post-write state.
#[derive(Clone, Debug)]
pub struct MouseSnapshot {
    pub path: String,
    pub firmware: (u8, u8),
    pub serial: String,
    pub mode: u8,
    pub polling_raw: u8,
    pub polling_hz: Option<u16>,
    pub dpi: (u16, u16),
    pub dpi_stages: DpiStages,
    pub brightness: u8,
}

#[derive(Clone, Debug)]
pub enum MouseStatus {
    Connecting,
    Connected(MouseSnapshot),
    Unavailable(String),
}

#[derive(Clone, Debug)]
pub enum MouseEvent {
    Status(MouseStatus),
    Busy(Option<String>),
    Result {
        label: String,
        detail: String,
        ok: bool,
    },
}

#[derive(Clone, Debug)]
pub enum MouseCommand {
    Refresh,
    SetPoll(u16),
    /// DPI writes request onboard storage; unplug/replug persistence is untested.
    SetDpi {
        x: u16,
        y: u16,
    },
    SetBrightness(u8),
    /// Acceptance acknowledges that colour cannot be verified or rolled back.
    SetStatic {
        color: Rgb,
        accepted: bool,
    },
    Stop,
}

/// Non-blocking command/event handle to the worker that holds the mouse open.
/// Commands submitted without a connection, or before a reconnect, are rejected.
pub struct MouseController {
    commands: Sender<QueuedCommand>,
    events: Receiver<MouseEvent>,
    thread: Option<thread::JoinHandle<()>>,
    connection: Arc<AtomicU64>,
}

impl MouseController {
    pub fn start() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let connection = Arc::new(AtomicU64::new(0));
        let worker_connection = Arc::clone(&connection);
        let worker_events = event_tx.clone();

        let _ = event_tx.send(MouseEvent::Status(MouseStatus::Connecting));
        let thread = match thread::Builder::new()
            .name("rgbd-mouse".to_string())
            .spawn(move || worker(command_rx, worker_events, worker_connection))
        {
            Ok(thread) => Some(thread),
            Err(err) => {
                let _ = event_tx.send(MouseEvent::Status(MouseStatus::Unavailable(format!(
                    "failed to start rgbd-mouse worker: {err}"
                ))));
                None
            }
        };

        Self {
            commands: command_tx,
            events: event_rx,
            thread,
            connection,
        }
    }

    pub fn refresh(&self) {
        self.send(MouseCommand::Refresh);
    }

    pub fn set_poll(&self, hz: u16) {
        self.send(MouseCommand::SetPoll(hz));
    }

    /// Request a guarded DPI change that asks for onboard storage.
    pub fn set_dpi(&self, x: u16, y: u16) {
        self.send(MouseCommand::SetDpi { x, y });
    }

    pub fn set_brightness(&self, value: u8) {
        self.send(MouseCommand::SetBrightness(value));
    }

    /// Request static colour only after accepting its unverifiable, irreversible nature.
    pub fn set_static(&self, color: Rgb, accepted: bool) {
        self.send(MouseCommand::SetStatic { color, accepted });
    }

    /// Poll one event without blocking. The GUI should drain events every repaint.
    pub fn poll_event(&self) -> Option<MouseEvent> {
        self.events.try_recv().ok()
    }

    /// Stop and join the worker. This can wait for an in-flight HID operation;
    /// dropping the controller instead requests shutdown without waiting.
    pub fn stop(&mut self) {
        self.send(MouseCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn send(&self, command: MouseCommand) {
        let _ = self.commands.send(QueuedCommand {
            command,
            connection: self.connection.load(Ordering::Acquire),
        });
    }
}

impl Drop for MouseController {
    fn drop(&mut self) {
        self.send(MouseCommand::Stop);
    }
}

// An open attempt can block while commands arrive. Tagging at enqueue time
// prevents those writes, and writes from an earlier connection, being replayed.
struct QueuedCommand {
    command: MouseCommand,
    connection: u64,
}

impl QueuedCommand {
    fn belongs_to(&self, connection: u64) -> bool {
        self.connection != 0 && self.connection == connection
    }
}

struct BusyGuard {
    events: Sender<MouseEvent>,
}

impl BusyGuard {
    fn begin(events: &Sender<MouseEvent>, label: &str) -> Option<Self> {
        // Do not begin USB work if the GUI has already dropped its receiver.
        events
            .send(MouseEvent::Busy(Some(label.to_string())))
            .ok()?;
        Some(Self {
            events: events.clone(),
        })
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        let _ = self.events.send(MouseEvent::Busy(None));
    }
}

struct Worker {
    events: Sender<MouseEvent>,
    connection: Arc<AtomicU64>,
    generation: u64,
    device: Option<RazerDevice>,
    next_attempt: Instant,
    last_unavailable: Option<String>,
}

fn worker(
    commands: Receiver<QueuedCommand>,
    events: Sender<MouseEvent>,
    connection: Arc<AtomicU64>,
) {
    let mut worker = Worker {
        events,
        connection,
        generation: 0,
        device: None,
        next_attempt: Instant::now(),
        last_unavailable: None,
    };

    loop {
        match commands.recv_timeout(Duration::from_millis(10)) {
            Ok(command) => {
                if !worker.handle(command) {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        if worker.device.is_none() && Instant::now() >= worker.next_attempt {
            let Some(_busy) = BusyGuard::begin(&worker.events, "Connecting") else {
                break;
            };
            worker.connect(false);
        }
    }
    worker.connection.store(0, Ordering::Release);
}

impl Worker {
    fn handle(&mut self, queued: QueuedCommand) -> bool {
        if matches!(queued.command, MouseCommand::Stop) {
            return false;
        }
        let label = command_label(&queued.command);
        let Some(_busy) = BusyGuard::begin(&self.events, label) else {
            return false;
        };

        if matches!(queued.command, MouseCommand::Refresh) {
            if self.device.is_some() {
                self.refresh_snapshot(true);
            } else {
                self.connect(true);
            }
            return true;
        }

        let Some(device) = self
            .device
            .as_ref()
            .filter(|_| queued.belongs_to(self.generation))
        else {
            report_result(
                &self.events,
                label,
                "Razer Viper Mini is not connected; wait for reconnect and refresh".to_string(),
                false,
            );
            return true;
        };

        if let Err(err) = run_command(device, &queued.command, &self.events) {
            report_result(&self.events, label, format!("{err:#}"), false);
        }
        // Always observe the actual state, including after a failed write's
        // rollback. The worker never retries a write or performs another rollback.
        self.refresh_snapshot(false);
        true
    }

    fn connect(&mut self, explicit_refresh: bool) {
        let opened = open_mouse().and_then(|device| {
            let snapshot = read_snapshot(&device)?;
            Ok((device, snapshot))
        });
        match opened {
            Ok((device, snapshot)) => {
                self.device = Some(device);
                self.generation = self.generation.wrapping_add(1).max(1);
                self.connection.store(self.generation, Ordering::Release);
                self.connected(snapshot);
            }
            Err(err) => {
                if explicit_refresh {
                    report_result(&self.events, "Refresh", format!("{err:#}"), false);
                }
                self.unavailable(&err, explicit_refresh);
            }
        }
    }

    fn refresh_snapshot(&mut self, explicit_refresh: bool) {
        let Some(device) = self.device.as_ref() else {
            return;
        };
        match read_snapshot(device) {
            Ok(snapshot) => self.connected(snapshot),
            Err(err) => {
                if explicit_refresh {
                    report_result(&self.events, "Refresh", format!("{err:#}"), false);
                }
                self.unavailable(&err, explicit_refresh);
            }
        }
    }

    fn connected(&mut self, snapshot: MouseSnapshot) {
        self.last_unavailable = None;
        let _ = self
            .events
            .send(MouseEvent::Status(MouseStatus::Connected(snapshot)));
    }

    fn unavailable(&mut self, err: &anyhow::Error, explicit_refresh: bool) {
        self.connection.store(0, Ordering::Release);
        self.device = None;
        self.next_attempt = Instant::now() + Duration::from_secs(1);
        let detail = format!("{err:#}");
        // Explicit refreshes get a status response; identical background retry
        // errors are suppressed until the error changes or a connection succeeds.
        if explicit_refresh || self.last_unavailable.as_deref() != Some(detail.as_str()) {
            let _ = self
                .events
                .send(MouseEvent::Status(MouseStatus::Unavailable(detail.clone())));
        }
        self.last_unavailable = Some(detail);
    }
}

fn read_snapshot(device: &RazerDevice) -> Result<MouseSnapshot> {
    let path = device.path.clone();
    let firmware = device.firmware().context("read mouse firmware")?;
    let serial = device.serial().context("read mouse serial")?;
    let mode = device.mode().context("read mouse mode")?;
    let (polling_raw, polling_hz) = device.poll_rate().context("read mouse polling rate")?;
    let dpi = device.dpi().context("read mouse DPI")?;
    let dpi_stages = device.dpi_stages().context("read mouse DPI stages")?;
    let brightness = device.logo_brightness().context("read mouse brightness")?;
    Ok(MouseSnapshot {
        path,
        firmware,
        serial,
        mode,
        polling_raw,
        polling_hz,
        dpi,
        dpi_stages,
        brightness,
    })
}

fn open_mouse() -> Result<RazerDevice> {
    let manager = HidManager::new().context("initialise mouse HID manager")?;
    match RazerDevice::open(&manager) {
        Err(err) if is_transient_open_error(&err) => {
            thread::sleep(Duration::from_millis(750));
            RazerDevice::open(&manager)
                .context("Razer Viper Mini open failed after one transient retry")
        }
        result => result,
    }
}

fn is_transient_open_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("0x00000079") || message.contains("semaphore timeout")
}

fn run_command(
    device: &RazerDevice,
    command: &MouseCommand,
    events: &Sender<MouseEvent>,
) -> Result<()> {
    validate_write_request(command)?;
    match *command {
        MouseCommand::SetPoll(hz) => {
            let (raw, current) = device.poll_rate().context("read current polling rate")?;
            let current = current.ok_or_else(|| {
                anyhow!(
                    "current polling rate byte 0x{raw:02X} cannot be safely restored; refusing to write"
                )
            })?;
            identity_then_target(current, hz, events, |value| {
                device.set_poll_rate_guarded(value)
            })
        }
        MouseCommand::SetDpi { x, y } => {
            let current = device.dpi().context("read current DPI")?;
            razer::dpi_write_args(current.0, current.1)
                .context("current DPI cannot be safely restored; refusing to write")?;
            // Both identity and target DPI writes request onboard storage.
            identity_then_target(current, (x, y), events, |(x, y)| {
                device.set_dpi_guarded(x, y)
            })
        }
        MouseCommand::SetBrightness(value) => {
            let current = device
                .logo_brightness()
                .context("read current logo brightness")?;
            identity_then_target(current, value, events, |value| {
                device.set_logo_brightness_guarded(value)
            })
        }
        MouseCommand::SetStatic { color, .. } => {
            let before = device
                .logo_brightness()
                .context("read brightness before static colour")?;
            device
                .set_logo_static(color.r, color.g, color.b)
                .context("static colour write failed; colour cannot be verified or rolled back")?;
            let after = device.logo_brightness().context(
                "read brightness after static colour; colour cannot be verified or rolled back",
            )?;
            if before != after {
                bail!(
                    "brightness changed {before}->{after}; colour cannot be verified or rolled back"
                );
            }
            report_result(
                events,
                "Static colour",
                format!(
                    "colour=#{:02X}{:02X}{:02X}; brightness {before}->{after}; \
                     colour cannot be verified or rolled back",
                    color.r, color.g, color.b
                ),
                true,
            );
            Ok(())
        }
        MouseCommand::Refresh | MouseCommand::Stop => Ok(()),
    }
}

/// Validate input and static-colour acceptance before any command USB traffic.
fn validate_write_request(command: &MouseCommand) -> Result<()> {
    match *command {
        MouseCommand::SetPoll(hz) => {
            razer::poll_rate_to_byte(hz)?;
        }
        MouseCommand::SetDpi { x, y } => {
            razer::dpi_write_args(x, y)?;
        }
        MouseCommand::SetStatic {
            accepted: false, ..
        } => bail!("static colour was not accepted; refusing to write"),
        _ => {}
    }
    Ok(())
}

/// A failed identity write aborts the target. The setter owns rollback; this
/// helper never retries either write, even if the setter restored the old value.
fn identity_then_target<T: Copy + Eq + Debug>(
    current: T,
    requested: T,
    events: &Sender<MouseEvent>,
    mut set: impl FnMut(T) -> Result<WriteOutcome<T>>,
) -> Result<()> {
    if current != requested {
        let outcome = set(current).context("identity preflight failed")?;
        report_result(events, "Identity preflight", format_outcome(&outcome), true);
    }
    let outcome = set(requested).context("target write failed")?;
    report_result(events, "Target write", format_outcome(&outcome), true);
    Ok(())
}

fn report_result(events: &Sender<MouseEvent>, label: &str, detail: String, ok: bool) {
    let _ = events.send(MouseEvent::Result {
        label: label.to_string(),
        detail,
        ok,
    });
}

fn format_outcome<T: Debug>(outcome: &WriteOutcome<T>) -> String {
    format!(
        "previous={:?}, requested={:?}, readback={:?}",
        outcome.previous, outcome.requested, outcome.readback
    )
}

fn command_label(command: &MouseCommand) -> &'static str {
    match command {
        MouseCommand::Refresh => "Refresh",
        MouseCommand::SetPoll(_) => "Polling rate",
        MouseCommand::SetDpi { .. } => "DPI",
        MouseCommand::SetBrightness(_) => "Brightness",
        MouseCommand::SetStatic { .. } => "Static colour",
        MouseCommand::Stop => "Stop",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_polling_outcome() {
        assert_eq!(
            format_outcome(&WriteOutcome {
                previous: 1000u16,
                requested: 500u16,
                readback: 500u16,
            }),
            "previous=1000, requested=500, readback=500"
        );
    }

    #[test]
    fn formats_dpi_outcome() {
        assert_eq!(
            format_outcome(&WriteOutcome {
                previous: (800u16, 800u16),
                requested: (1600u16, 1200u16),
                readback: (1600u16, 1200u16),
            }),
            "previous=(800, 800), requested=(1600, 1200), readback=(1600, 1200)"
        );
    }

    #[test]
    fn recognises_transient_open_errors_in_the_error_chain() {
        assert!(is_transient_open_error(&anyhow!(
            "The semaphore timeout period has expired"
        )));
        assert!(is_transient_open_error(&anyhow!("HID error 0x00000079")));
        assert!(is_transient_open_error(
            &anyhow!("SEMAPHORE TIMEOUT").context("failed to open collection")
        ));
        assert!(!is_transient_open_error(&anyhow!("device not found")));
        assert!(!is_transient_open_error(&anyhow!("HID error 0x00000005")));
    }

    #[test]
    fn labels_commands() {
        for (command, label) in [
            (MouseCommand::Refresh, "Refresh"),
            (MouseCommand::SetPoll(1000), "Polling rate"),
            (MouseCommand::SetDpi { x: 800, y: 800 }, "DPI"),
            (MouseCommand::SetBrightness(255), "Brightness"),
            (
                MouseCommand::SetStatic {
                    color: Rgb::RED,
                    accepted: true,
                },
                "Static colour",
            ),
            (MouseCommand::Stop, "Stop"),
        ] {
            assert_eq!(command_label(&command), label);
        }
    }

    #[test]
    fn validates_write_requests_without_hardware() {
        assert!(validate_write_request(&MouseCommand::SetPoll(250)).is_err());
        assert!(validate_write_request(&MouseCommand::SetPoll(1000)).is_ok());
        assert!(validate_write_request(&MouseCommand::SetDpi { x: 99, y: 800 }).is_err());
        assert!(validate_write_request(&MouseCommand::SetDpi { x: 800, y: 8501 }).is_err());
        assert!(validate_write_request(&MouseCommand::SetDpi { x: 100, y: 8500 }).is_ok());
        let err = validate_write_request(&MouseCommand::SetStatic {
            color: Rgb::BLUE,
            accepted: false,
        })
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "static colour was not accepted; refusing to write"
        );
        assert!(
            validate_write_request(&MouseCommand::SetStatic {
                color: Rgb::BLUE,
                accepted: true,
            })
            .is_ok()
        );
    }

    #[test]
    fn identity_preflight_precedes_target_write() {
        let (events, received) = mpsc::channel();
        let mut writes = Vec::new();
        identity_then_target(1000u16, 500, &events, |value| {
            writes.push(value);
            Ok(WriteOutcome {
                previous: 1000,
                requested: value,
                readback: value,
            })
        })
        .unwrap();

        assert_eq!(writes, [1000, 500]);
        let labels: Vec<_> = received
            .try_iter()
            .map(|event| match event {
                MouseEvent::Result {
                    label, ok: true, ..
                } => label,
                event => panic!("unexpected event: {event:?}"),
            })
            .collect();
        assert_eq!(labels, ["Identity preflight", "Target write"]);
    }

    #[test]
    fn unchanged_value_still_gets_one_target_write() {
        let (events, received) = mpsc::channel();
        let mut writes = Vec::new();
        identity_then_target(255u8, 255, &events, |value| {
            writes.push(value);
            Ok(WriteOutcome {
                previous: value,
                requested: value,
                readback: value,
            })
        })
        .unwrap();

        assert_eq!(writes, [255]);
        assert!(matches!(
            received.try_recv().unwrap(),
            MouseEvent::Result { label, ok: true, .. } if label == "Target write"
        ));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn failed_identity_preflight_aborts_target_without_retry() {
        let (events, received) = mpsc::channel();
        let mut writes = Vec::new();
        let err = identity_then_target((800u16, 800u16), (1600, 1600), &events, |value| {
            writes.push(value);
            bail!("guard failed; rollback succeeded=true");
        })
        .unwrap_err();

        assert_eq!(writes, [(800, 800)]);
        assert!(format!("{err:#}").contains("identity preflight failed"));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn failed_target_is_not_retried() {
        let (events, received) = mpsc::channel();
        let mut writes = Vec::new();
        let err = identity_then_target(1000u16, 500, &events, |value| {
            writes.push(value);
            if value == 500 {
                bail!("target readback mismatch; rollback succeeded=true");
            }
            Ok(WriteOutcome {
                previous: 1000,
                requested: value,
                readback: value,
            })
        })
        .unwrap_err();

        assert_eq!(writes, [1000, 500]);
        assert!(format!("{err:#}").contains("target write failed"));
        assert!(matches!(
            received.try_recv().unwrap(),
            MouseEvent::Result { label, ok: true, .. } if label == "Identity preflight"
        ));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn commands_cannot_cross_a_reconnect() {
        let offline = QueuedCommand {
            command: MouseCommand::SetPoll(500),
            connection: 0,
        };
        assert!(!offline.belongs_to(0));
        assert!(!offline.belongs_to(1));

        let connected = QueuedCommand {
            command: MouseCommand::SetPoll(500),
            connection: 1,
        };
        assert!(connected.belongs_to(1));
        assert!(!connected.belongs_to(0));
        assert!(!connected.belongs_to(2));
    }

    // Construct only the disconnected state. No test starts a worker or opens HID.
    fn disconnected_worker() -> (Worker, Receiver<MouseEvent>) {
        let (events, received) = mpsc::channel();
        (
            Worker {
                events,
                connection: Arc::new(AtomicU64::new(0)),
                generation: 0,
                device: None,
                next_attempt: Instant::now(),
                last_unavailable: None,
            },
            received,
        )
    }

    #[test]
    fn disconnected_writes_emit_busy_failure_and_idle() {
        let (mut worker, received) = disconnected_worker();
        for command in [
            MouseCommand::SetPoll(500),
            MouseCommand::SetDpi { x: 1600, y: 1600 },
            MouseCommand::SetBrightness(128),
            MouseCommand::SetStatic {
                color: Rgb::GREEN,
                accepted: true,
            },
        ] {
            let expected_label = command_label(&command);
            assert!(worker.handle(QueuedCommand {
                command,
                connection: 0,
            }));
            assert!(matches!(
                received.try_recv().unwrap(),
                MouseEvent::Busy(Some(label)) if label == expected_label
            ));
            assert!(matches!(
                received.try_recv().unwrap(),
                MouseEvent::Result { label, detail, ok: false }
                    if label == expected_label
                        && detail == "Razer Viper Mini is not connected; wait for reconnect and refresh"
            ));
            assert!(matches!(
                received.try_recv().unwrap(),
                MouseEvent::Busy(None)
            ));
            assert!(received.try_recv().is_err());
        }
    }

    #[test]
    fn busy_guard_clears_on_error() {
        let (events, received) = mpsc::channel();
        let operation = || -> Result<()> {
            let _busy = BusyGuard::begin(&events, "DPI").unwrap();
            bail!("simulated error");
        };
        assert!(operation().is_err());
        assert!(matches!(
            received.try_recv().unwrap(),
            MouseEvent::Busy(Some(label)) if label == "DPI"
        ));
        assert!(matches!(
            received.try_recv().unwrap(),
            MouseEvent::Busy(None)
        ));
    }

    #[test]
    fn identical_background_errors_are_suppressed() {
        let (mut worker, received) = disconnected_worker();
        let before = Instant::now();
        worker.unavailable(&anyhow!("mouse missing"), false);
        worker.unavailable(&anyhow!("mouse missing"), false);
        worker.unavailable(&anyhow!("mouse busy"), false);
        let errors: Vec<_> = received
            .try_iter()
            .map(|event| match event {
                MouseEvent::Status(MouseStatus::Unavailable(detail)) => detail,
                event => panic!("unexpected event: {event:?}"),
            })
            .collect();
        assert_eq!(errors, ["mouse missing", "mouse busy"]);
        assert_eq!(worker.connection.load(Ordering::Acquire), 0);
        assert!(worker.next_attempt >= before + Duration::from_secs(1));
    }

    #[test]
    fn dropping_controller_sends_stop_without_starting_hardware() {
        let (commands, received) = mpsc::channel();
        let (_events, event_rx) = mpsc::channel();
        let controller = MouseController {
            commands,
            events: event_rx,
            thread: None,
            connection: Arc::new(AtomicU64::new(0)),
        };
        drop(controller);
        assert!(matches!(
            received.try_recv().unwrap().command,
            MouseCommand::Stop
        ));
    }
}
