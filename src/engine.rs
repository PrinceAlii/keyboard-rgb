//! Background HID worker.
//!
//! The GUI never touches the keyboard directly: opening the device, pacing
//! frames, and reconnecting after a cable pull all happen on one worker thread.
//! The UI only pushes the latest frame and reads status, so a slow or missing
//! keyboard can never stall a repaint.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::color::Rgb;
use crate::hid::HidManager;
use crate::sinowealth::{KEEPALIVE_MS, Keyboard, MAX_FPS, MIN_FRAME_GAP_MS};

/// A frame is one RGB value per streaming slot.
pub type Frame = Vec<Rgb>;

#[derive(Clone, Debug)]
pub enum Status {
    Connecting,
    Connected {
        path: String,
        model_id: Option<u8>,
        mode_bytes: Option<[u8; 3]>,
    },
    Unavailable(String),
}

pub enum Command {
    Frame(Frame),
    Stop,
}

/// Handle to the worker thread.
pub struct Engine {
    commands: Sender<Command>,
    status: Receiver<Status>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Engine {
    pub fn start() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::channel();

        let thread = thread::Builder::new()
            .name("rgbd-hid".to_string())
            .spawn(move || worker(command_rx, status_tx))
            .ok();

        Self {
            commands: command_tx,
            status: status_rx,
            thread,
        }
    }

    /// Push the newest frame. The worker replaces any pending frame with this
    /// one, so the UI can never build a backlog of stale lighting states.
    pub fn send_frame(&self, frame: Frame) {
        let _ = self.commands.send(Command::Frame(frame));
    }

    /// Read the latest status without blocking.
    pub fn poll_status(&self) -> Option<Status> {
        self.status.try_recv().ok()
    }

    pub fn stop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}

fn worker(commands: Receiver<Command>, status: Sender<Status>) {
    let gap = Duration::from_millis(MIN_FRAME_GAP_MS.max(1000 / u64::from(MAX_FPS)));
    let keepalive = Duration::from_millis(KEEPALIVE_MS);

    let mut keyboard: Option<Keyboard> = None;
    let mut frame: Option<Frame> = None;
    let mut last_sent: Option<Frame> = None;
    let mut last_sent_at: Option<Instant> = None;
    let mut next_write = Instant::now();
    let mut next_attempt = Instant::now();

    let _ = status.send(Status::Connecting);

    loop {
        match commands.recv_timeout(Duration::from_millis(10)) {
            Ok(Command::Frame(new_frame)) => frame = Some(new_frame),
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        // Connect lazily and retry slowly. A missing keyboard is a normal state
        // for a lighting app, not an error the UI should spin on.
        if keyboard.is_none() && Instant::now() >= next_attempt {
            next_attempt = Instant::now() + Duration::from_secs(1);
            match open_keyboard() {
                Ok(device) => {
                    let _ = status.send(Status::Connected {
                        path: device.path.clone(),
                        model_id: device.model_id,
                        mode_bytes: device.mode_bytes(),
                    });
                    keyboard = Some(device);
                    // Repaint immediately after a reconnect; the board forgets
                    // 0x08 frames when the link drops.
                    last_sent = None;
                    last_sent_at = None;
                    next_write = Instant::now();
                }
                Err(err) => {
                    let _ = status.send(Status::Unavailable(format!("{err:#}")));
                }
            }
        }

        let Some(device) = keyboard.as_ref() else {
            continue;
        };

        // Opening the device can block. Frames that arrived while it was
        // opening are still queued, so keep only the newest one; otherwise the
        // reconnect would repaint an obsolete colour before reading them.
        loop {
            match commands.try_recv() {
                Ok(Command::Frame(new_frame)) => frame = Some(new_frame),
                Ok(Command::Stop) => return,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        let Some(current) = frame.as_ref() else {
            continue;
        };

        if Instant::now() < next_write {
            continue;
        }

        // `0x08` is a volatile framebuffer, not persistent configuration. Send
        // a frame when it changes, then refresh an unchanged frame at the
        // keepalive interval. OpenRGB's working F99 driver does the same. This
        // keeps static colours solid without writing USB every 50 ms.
        let changed = last_sent.as_deref() != Some(current.as_slice());
        let keepalive_due = last_sent_at.is_none_or(|sent| sent.elapsed() >= keepalive);
        if !changed && !keepalive_due {
            continue;
        }

        match device.send_stream(current) {
            Ok(()) => {
                last_sent = Some(current.clone());
                last_sent_at = Some(Instant::now());
                next_write = Instant::now() + gap;
            }
            Err(err) => {
                let _ = status.send(Status::Unavailable(format!("{err:#}")));
                keyboard = None;
                next_attempt = Instant::now() + Duration::from_secs(1);
            }
        }
    }
}

fn open_keyboard() -> anyhow::Result<Keyboard> {
    let manager = HidManager::new()?;
    Keyboard::open(&manager)
}
