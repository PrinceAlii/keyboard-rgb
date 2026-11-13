//! egui front end.
//!
//! One window, five tabs, and a live preview of the board drawn from the
//! vendor's own key coordinates. Remapping is not implemented.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::color::Rgb;
use crate::conflicts::{Conflict, ConflictMonitor};
use crate::engine::{Engine, Frame as StreamFrame, Status};
use crate::layout::{F99_KEYS, LAYOUT_HEIGHT, LAYOUT_WIDTH};
use crate::mouse::{MouseController, MouseEvent, MouseSnapshot, MouseStatus};
use crate::razer;
use crate::sinowealth::{MODE_PER_KEY, STREAM_SLOTS, model_name};
use crate::startup;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, PointerButton, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, Vec2,
};

const ACCENT: Color32 = Color32::from_rgb(94, 234, 212);
const MUTED: Color32 = Color32::from_rgb(136, 146, 162);
const OK: Color32 = Color32::from_rgb(74, 222, 128);
const WARN: Color32 = Color32::from_rgb(251, 191, 36);
const BAD: Color32 = Color32::from_rgb(248, 113, 113);
const KEY_EDGE: Color32 = Color32::from_rgb(38, 43, 52);
const PANEL: Color32 = Color32::from_rgb(17, 20, 25);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Lighting,
    Remap,
    Mouse,
    Settings,
    Diagnostics,
}

struct MouseLogEntry {
    ok: bool,
    label: String,
    detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Effect {
    Static,
    Rainbow,
    Wave,
    Breathing,
}

impl Effect {
    const ALL: [Self; 4] = [Self::Static, Self::Rainbow, Self::Wave, Self::Breathing];

    fn label(self) -> &'static str {
        match self {
            Self::Static => "Static / per-key",
            Self::Rainbow => "Rainbow",
            Self::Wave => "Wave",
            Self::Breathing => "Breathing",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Rainbow => "rainbow",
            Self::Wave => "wave",
            Self::Breathing => "breathing",
        }
    }

    fn from_key(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|effect| effect.key() == value)
    }

    fn animated(self) -> bool {
        !matches!(self, Self::Static)
    }
}

pub struct RgbdApp {
    tab: Tab,
    colors: StreamFrame,
    color: Rgb,
    brightness: u8,
    effect: Effect,
    speed: f32,

    engine: Engine,
    status: Status,
    last_sent: Option<StreamFrame>,
    conflicts: Vec<Conflict>,
    conflict_monitor: ConflictMonitor,

    mouse: MouseController,
    mouse_status: MouseStatus,
    mouse_busy: Option<String>,
    mouse_log: Vec<MouseLogEntry>,
    mouse_poll: u16,
    mouse_dpi_x: u16,
    mouse_dpi_y: u16,
    mouse_dpi_link: bool,
    mouse_brightness: u8,
    mouse_static_color: Rgb,
    mouse_pending_static: Option<Rgb>,
    mouse_static_ack: bool,

    startup_status: Option<startup::Status>,
    startup_error: Option<String>,
    folder_error: Option<String>,
    save_error: Option<String>,
    diagnostics_copied: Option<Instant>,

    started: Instant,
    dirty: bool,
    last_save: Instant,
}

impl RgbdApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_style(&cc.egui_ctx);

        let saved = Persisted::load();
        let colors = saved
            .as_ref()
            .map(|state| state.colors.clone())
            .unwrap_or_else(|| vec![Rgb::BLACK; STREAM_SLOTS]);

        Self {
            tab: Tab::Lighting,
            colors,
            color: saved
                .as_ref()
                .map(|state| state.color)
                .unwrap_or(Rgb::new(0, 229, 255)),
            brightness: saved.as_ref().map(|state| state.brightness).unwrap_or(220),
            effect: saved
                .as_ref()
                .map(|state| state.effect)
                .unwrap_or(Effect::Static),
            speed: saved.as_ref().map(|state| state.speed).unwrap_or(1.0),
            engine: Engine::start(),
            status: Status::Connecting,
            last_sent: None,
            conflicts: Vec::new(),
            conflict_monitor: ConflictMonitor::start(),
            mouse: MouseController::start(),
            mouse_status: MouseStatus::Connecting,
            mouse_busy: None,
            mouse_log: Vec::new(),
            mouse_poll: 1000,
            mouse_dpi_x: 800,
            mouse_dpi_y: 800,
            mouse_dpi_link: true,
            mouse_brightness: 255,
            mouse_static_color: Rgb::new(0, 255, 0),
            mouse_pending_static: None,
            mouse_static_ack: false,
            startup_status: None,
            startup_error: None,
            folder_error: None,
            save_error: None,
            diagnostics_copied: None,
            started: Instant::now(),
            dirty: false,
            last_save: Instant::now(),
        }
    }

    fn preview_frame(&self) -> StreamFrame {
        let time = self.started.elapsed().as_secs_f32();
        let mut frame = vec![Rgb::BLACK; STREAM_SLOTS];

        match self.effect {
            Effect::Static => frame.clone_from(&self.colors),
            Effect::Rainbow => {
                for key in &F99_KEYS {
                    let hue = key.x1 / LAYOUT_WIDTH * 0.85 + time * self.speed * 0.12;
                    frame[key.led as usize] = hsv_to_rgb(hue.rem_euclid(1.0), 0.92, 1.0);
                }
            }
            Effect::Wave => {
                for key in &F99_KEYS {
                    let phase = key.x1 / LAYOUT_WIDTH * 7.0 - time * self.speed * 3.4;
                    let level = 0.28 + 0.72 * (0.5 + 0.5 * phase.sin());
                    frame[key.led as usize] = self.color.scaled(level);
                }
            }
            Effect::Breathing => {
                let level = 0.18 + 0.82 * (0.5 + 0.5 * (time * self.speed * 1.9).sin());
                frame.fill(self.color.scaled(level));
            }
        }

        let brightness = f32::from(self.brightness) / 255.0;
        for color in &mut frame {
            *color = color.scaled(brightness);
        }
        frame
    }

    fn push_frame(&mut self, ctx: &egui::Context) {
        let frame = self.preview_frame();
        let changed = self.last_sent.as_deref() != Some(frame.as_slice());

        if changed {
            self.engine.send_frame(frame.clone());
            self.last_sent = Some(frame);
        }

        if self.effect.animated() {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn save_if_due(&mut self) {
        if !self.dirty || self.last_save.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.save_now();
    }

    fn save_now(&mut self) {
        let state = Persisted {
            colors: self.colors.clone(),
            color: self.color,
            brightness: self.brightness,
            effect: self.effect,
            speed: self.speed,
        };
        match state.save() {
            Ok(()) => {
                self.dirty = false;
                self.save_error = None;
            }
            Err(err) => {
                self.save_error = Some(format!("{err:#}"));
            }
        }
        self.last_save = Instant::now();
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("rgbd").size(22.0).strong().color(ACCENT));
                ui.label(
                    RichText::new("AULA F99")
                        .size(11.0)
                        .color(MUTED)
                        .text_style(egui::TextStyle::Small),
                );
                ui.add_space(26.0);

                for (tab, label) in [
                    (Tab::Lighting, "Lighting"),
                    (Tab::Remap, "Remap"),
                    (Tab::Mouse, "Mouse"),
                    (Tab::Settings, "Settings"),
                    (Tab::Diagnostics, "Diagnostics"),
                ] {
                    if ui
                        .selectable_label(self.tab == tab, RichText::new(label).size(14.0))
                        .clicked()
                    {
                        self.tab = tab;
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.status_pill(ui);
                });
            });
            ui.add_space(6.0);
        });
    }

    fn status_pill(&self, ui: &mut egui::Ui) {
        let (color, text) = if !self.conflicts.is_empty() {
            (BAD, "Contention".to_string())
        } else {
            match &self.status {
                Status::Connecting => (WARN, "Connecting".to_string()),
                Status::Connected {
                    mode_bytes: Some(mode),
                    ..
                } if *mode == MODE_PER_KEY => (OK, "Connected".to_string()),
                Status::Connected { .. } => (WARN, "Effect mode".to_string()),
                Status::Unavailable(_) => (BAD, "Not connected".to_string()),
            }
        };

        ui.label(RichText::new("●").color(color).size(13.0));
        ui.label(RichText::new(text).size(12.0).color(MUTED));
    }

    fn conflict_names(&self) -> String {
        self.conflicts
            .iter()
            .map(|conflict| conflict.label)
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn conflict_banner(&self, ui: &mut egui::Ui) {
        if self.conflicts.is_empty() {
            return;
        }

        egui::Frame::new()
            .fill(Color32::from_rgb(42, 18, 20))
            .stroke(Stroke::new(1.0, BAD.gamma_multiply(0.7)))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(12, 9))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} is also driving this keyboard",
                            self.conflict_names()
                        ))
                        .size(13.0)
                        .strong()
                        .color(BAD),
                    );
                    ui.label(
                        RichText::new(
                            "· two writers alternate frames, which makes the board flicker. \
                             Close the other program before using rgbd.",
                        )
                        .size(12.0)
                        .color(MUTED),
                    );
                });
            });
        ui.add_space(8.0);
    }

    fn lighting_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        self.conflict_banner(ui);

        let controls_height = 188.0;
        let canvas_height = (ui.available_height() - controls_height - 10.0).max(220.0);
        let canvas_width = ui.available_width();
        self.keyboard_canvas(ui, Vec2::new(canvas_width, canvas_height));

        ui.add_space(10.0);
        ui.horizontal_top(|ui| {
            let total = ui.available_width();
            let gap = 12.0;
            ui.spacing_mut().item_spacing.x = gap;

            let colour_width = 288.0_f32.min(total * 0.27);
            let effect_width = 380.0_f32.min(total * 0.35);

            card(ui, colour_width, |ui| self.colour_controls(ui));
            card(ui, effect_width, |ui| self.effect_controls(ui));

            // Measure what is actually left instead of predicting it: the
            // parent row adds its own item spacing between frames.
            let board_width = (ui.available_width() - 4.0).max(180.0);
            card(ui, board_width, |ui| self.board_controls(ui));
        });

        ui.add_space(2.0);
        ui.label(
            RichText::new("Left-click or drag to paint · Right-click a key to pick its colour")
                .size(11.0)
                .color(MUTED),
        );
    }

    fn keyboard_canvas(&mut self, ui: &mut egui::Ui, size: Vec2) {
        let (response, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let rect = response.rect;
        painter.rect_filled(rect, CornerRadius::same(12), Color32::from_rgb(10, 12, 16));

        let scale = ((rect.width() - 44.0) / LAYOUT_WIDTH)
            .min((rect.height() - 52.0) / LAYOUT_HEIGHT)
            .max(0.24);
        let origin = Pos2::new(
            rect.center().x - LAYOUT_WIDTH * scale * 0.5,
            rect.center().y - LAYOUT_HEIGHT * scale * 0.5 + 4.0,
        );

        // A slightly lighter plate behind the keys gives the board an edge
        // instead of letting black keys disappear into the background.
        let plate = Rect::from_min_max(
            origin,
            Pos2::new(
                origin.x + LAYOUT_WIDTH * scale,
                origin.y + LAYOUT_HEIGHT * scale,
            ),
        )
        .expand(8.0);
        painter.rect_filled(plate, CornerRadius::same(10), Color32::from_rgb(15, 18, 23));
        painter.rect_stroke(
            plate,
            CornerRadius::same(10),
            Stroke::new(1.0, Color32::from_rgb(30, 35, 43)),
            StrokeKind::Inside,
        );

        painter.text(
            Pos2::new(rect.left() + 14.0, rect.top() + 10.0),
            Align2::LEFT_TOP,
            "AULA F99  ·  99 keys",
            FontId::proportional(10.0),
            MUTED,
        );
        let (live_label, live_color) = if !self.conflicts.is_empty() {
            ("CONTENTION · CLOSE THE OTHER APP", BAD)
        } else if self.effect.animated() {
            ("LIVE · 20 FPS", ACCENT)
        } else {
            ("LIVE · CHANGE-ONLY · 1 S KEEPALIVE", ACCENT)
        };
        painter.text(
            Pos2::new(rect.right() - 14.0, rect.top() + 10.0),
            Align2::RIGHT_TOP,
            live_label,
            FontId::proportional(10.0),
            live_color,
        );

        let frame = self.preview_frame();
        let hover = response.hover_pos();

        for key in &F99_KEYS {
            let key_rect = key_rect(origin, scale, key.x1, key.y1, key.x2, key.y2);
            let is_hovered = hover.is_some_and(|pos| key_rect.contains(pos));
            let color = frame[key.led as usize];
            draw_key(&painter, key_rect, key.label, color, is_hovered, scale);
        }

        // Paint on primary click/drag, pick on secondary click.
        let pointer = response.interact_pointer_pos();
        if (response.clicked_by(PointerButton::Primary)
            || response.dragged_by(PointerButton::Primary))
            && let Some(pos) = pointer
            && let Some(key) = self.key_at(origin, scale, pos)
        {
            self.colors[key.led as usize] = self.color;
            self.dirty = true;
            self.last_save = Instant::now();
        }

        if (response.secondary_clicked() || response.dragged_by(PointerButton::Secondary))
            && let Some(pos) = pointer
            && let Some(key) = self.key_at(origin, scale, pos)
        {
            self.color = self.colors[key.led as usize];
        }
    }

    fn key_at(
        &self,
        origin: Pos2,
        scale: f32,
        pos: Pos2,
    ) -> Option<&'static crate::layout::KeyDef> {
        F99_KEYS
            .iter()
            .find(|key| key_rect(origin, scale, key.x1, key.y1, key.x2, key.y2).contains(pos))
    }

    fn colour_controls(&mut self, ui: &mut egui::Ui) {
        section(ui, "COLOUR");
        ui.horizontal(|ui| {
            let mut picked = self.color.to_color32();
            if ui.color_edit_button_srgba(&mut picked).changed() {
                self.color = Rgb::from_color32(picked);
            }
            ui.label(RichText::new(self.color.to_hex()).monospace().size(12.0));
        });

        ui.add_space(6.0);
        for row in PALETTE.chunks(5) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                for preset in row {
                    let (swatch, response) =
                        ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                    ui.painter().rect_filled(
                        swatch,
                        CornerRadius::same(4),
                        Color32::from_rgb(preset.r, preset.g, preset.b),
                    );
                    ui.painter().rect_stroke(
                        swatch,
                        CornerRadius::same(4),
                        Stroke::new(1.0, Color32::from_rgb(48, 54, 64)),
                        StrokeKind::Inside,
                    );
                    if response.clicked() {
                        self.color = *preset;
                    }
                    if response.hovered() {
                        ui.painter().rect_stroke(
                            swatch,
                            CornerRadius::same(4),
                            Stroke::new(1.5, ACCENT),
                            StrokeKind::Inside,
                        );
                    }
                }
            });
        }
    }

    fn effect_controls(&mut self, ui: &mut egui::Ui) {
        section(ui, "EFFECT");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("effect")
                .width(176.0)
                .selected_text(self.effect.label())
                .show_ui(ui, |ui| {
                    for effect in Effect::ALL {
                        ui.selectable_value(&mut self.effect, effect, effect.label());
                    }
                });

            if self.effect.animated() {
                ui.add(
                    egui::Slider::new(&mut self.speed, 0.2..=2.5)
                        .text("Speed")
                        .fixed_decimals(2),
                );
            }
        });

        ui.add_space(4.0);
        let mut brightness = u32::from(self.brightness) * 100 / 255;
        if ui
            .add(
                egui::Slider::new(&mut brightness, 0..=100)
                    .suffix("%")
                    .text("Brightness"),
            )
            .changed()
        {
            self.brightness = (brightness * 255 / 100) as u8;
            self.dirty = true;
            self.last_save = Instant::now();
        }
    }

    fn board_controls(&mut self, ui: &mut egui::Ui) {
        section(ui, "BOARD");
        ui.horizontal(|ui| {
            if ui.button("Fill all").clicked() {
                self.colors.fill(self.color);
                self.dirty = true;
                self.last_save = Instant::now();
            }
            if ui.button("Turn off").clicked() {
                self.colors.fill(Rgb::BLACK);
                self.dirty = true;
                self.last_save = Instant::now();
            }
        });

        ui.add_space(8.0);
        match &self.status {
            Status::Connecting => {
                ui.label(
                    RichText::new("Looking for the keyboard…")
                        .size(12.0)
                        .color(MUTED),
                );
            }
            Status::Connected {
                path,
                model_id,
                mode_bytes,
            } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(OK).size(11.0));
                    ui.label(RichText::new("AULA F99 connected").size(12.0).color(OK));
                });
                let mut detail = String::new();
                if let Some(id) = model_id {
                    detail.push_str(&format!("model 0x{id:02X} · {}", model_name(*id)));
                }
                if let Some(mode) = mode_bytes {
                    if !detail.is_empty() {
                        detail.push_str(" · ");
                    }
                    detail.push_str(&format!(
                        "mode {:02X} {:02X} {:02X}",
                        mode[0], mode[1], mode[2]
                    ));
                }
                if !detail.is_empty() {
                    ui.label(RichText::new(detail).size(10.5).color(MUTED));
                }
                if *mode_bytes != Some(MODE_PER_KEY) {
                    ui.label(
                        RichText::new(
                            "Not in per-key mode: a firmware effect will repaint over streamed \
                             colours. Switch it once with `rgbctl per-key-mode`.",
                        )
                        .size(10.5)
                        .color(WARN),
                    );
                }
                ui.label(
                    RichText::new(short_path(path))
                        .size(10.0)
                        .monospace()
                        .color(MUTED),
                )
                .on_hover_text(path);
            }
            Status::Unavailable(err) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(BAD).size(11.0));
                    ui.label(RichText::new("Keyboard unavailable").size(12.0).color(BAD));
                });
                ui.label(RichText::new(first_line(err)).size(10.5).color(MUTED))
                    .on_hover_text(err);
            }
        }
    }

    fn placeholder_tab(&self, ui: &mut egui::Ui, title: &str, body: &[&str]) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(title).size(22.0).strong());
            ui.add_space(6.0);
            for paragraph in body {
                ui.label(RichText::new(*paragraph).size(13.0).color(MUTED));
                ui.add_space(4.0);
            }
        });
    }

    fn remap_tab(&mut self, ui: &mut egui::Ui) {
        self.placeholder_tab(
            ui,
            "Onboard remapping",
            &[
                "Key remapping is not implemented.",
                "rgbd does not install a keyboard hook.",
            ],
        );
    }

    fn mouse_tab(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(8.0);
            self.mouse_status_card(ui);
            ui.add_space(10.0);

            egui::Frame::new()
                .fill(Color32::from_rgb(40, 34, 19))
                .stroke(Stroke::new(1.0, WARN.gamma_multiply(0.65)))
                .corner_radius(CornerRadius::same(9))
                .inner_margin(egui::Margin::symmetric(12, 9))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(
                            "Direct HID control; no Razer Synapse dependency. Close Razer Synapse, SignalRGB, and OpenRGB before writing. Polling, DPI, and brightness are read back, with a rollback attempt on mismatch. Static colour cannot be read back or rolled back.",
                        )
                        .size(12.0)
                        .color(WARN),
                    );
                    if !self.conflicts.is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!(
                                "Detected conflicts: {}. Close these before writing.",
                                self.conflict_names()
                            ))
                            .size(12.0)
                            .strong()
                            .color(BAD),
                        );
                    }
                });

            ui.add_space(10.0);
            let gap = ui.spacing().item_spacing.x;
            let card_width = ((ui.available_width() - gap * 2.0) / 3.0).max(148.0);
            ui.horizontal_top(|ui| {
                self.mouse_poll_card(ui, card_width);
                self.mouse_dpi_card(ui, card_width);
                self.mouse_logo_card(ui, card_width);
            });

            ui.add_space(10.0);
            self.mouse_details_card(ui);
            ui.add_space(10.0);
            self.mouse_log_card(ui);
            ui.add_space(8.0);
        });
    }

    fn mouse_status_card(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let status = self.mouse_status.clone();
        let busy = self.mouse_busy.clone();
        let modal_pending = self.mouse_pending_static.is_some();
        let mut refresh = false;

        card(ui, width, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| match &status {
                    MouseStatus::Connecting => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(WARN).size(12.0));
                            ui.label(
                                RichText::new("Looking for the Razer Viper Mini…")
                                    .size(14.0)
                                    .strong()
                                    .color(WARN),
                            );
                        });
                    }
                    MouseStatus::Connected(snapshot) => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(OK).size(12.0));
                            ui.label(
                                RichText::new("Razer Viper Mini connected")
                                    .size(14.0)
                                    .strong()
                                    .color(OK),
                            );
                        });
                        ui.label(
                            RichText::new(format!(
                                "Firmware v{}.{} · serial {}",
                                snapshot.firmware.0, snapshot.firmware.1, snapshot.serial
                            ))
                            .size(11.0)
                            .color(MUTED),
                        );
                        ui.label(
                            RichText::new(short_razer_path(&snapshot.path))
                                .monospace()
                                .size(10.5),
                        )
                        .on_hover_text(&snapshot.path);
                    }
                    MouseStatus::Unavailable(error) => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(BAD).size(12.0));
                            ui.label(
                                RichText::new("Razer Viper Mini unavailable")
                                    .size(14.0)
                                    .strong()
                                    .color(BAD),
                            );
                        });
                        ui.label(RichText::new(first_line(error)).size(11.0).color(MUTED))
                            .on_hover_text(error);
                        ui.label(
                            RichText::new("Retrying automatically.")
                                .size(10.5)
                                .color(MUTED),
                        );
                    }
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    refresh = ui
                        .add_enabled(
                            busy.is_none() && !modal_pending,
                            egui::Button::new("Refresh"),
                        )
                        .clicked();
                    if let Some(label) = &busy {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new(label).size(11.0).color(MUTED));
                        });
                    }
                });
            });
        });

        if refresh {
            self.mouse.refresh();
            self.mouse_busy = Some("Refresh".to_string());
        }
    }

    fn mouse_poll_card(&mut self, ui: &mut egui::Ui, width: f32) {
        let can_write = self.mouse_controls_enabled();
        let current = self
            .mouse_snapshot()
            .map(|snapshot| (snapshot.polling_raw, snapshot.polling_hz));

        card(ui, width, |ui| {
            section(ui, "POLLING RATE");
            match current {
                Some((_, Some(hz))) => {
                    ui.label(
                        RichText::new(format!("Current: {hz} Hz"))
                            .size(11.0)
                            .color(MUTED),
                    );
                }
                Some((raw, None)) => {
                    ui.label(
                        RichText::new(format!("Unknown byte 0x{raw:02X}; writes disabled"))
                            .size(11.0)
                            .color(WARN),
                    );
                }
                None => {
                    ui.label(
                        RichText::new("Current: unavailable")
                            .size(11.0)
                            .color(MUTED),
                    );
                }
            }

            ui.add_enabled_ui(can_write, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for hz in [125, 500, 1000] {
                        ui.selectable_value(&mut self.mouse_poll, hz, format!("{hz} Hz"));
                    }
                });
            });

            let changed = current.is_some_and(|(_, hz)| hz.is_some_and(|hz| hz != self.mouse_poll));
            let apply = ui
                .add_enabled(can_write && changed, egui::Button::new("Apply"))
                .clicked();
            if apply {
                self.mouse.set_poll(self.mouse_poll);
                self.mouse_busy = Some("Polling rate".to_string());
            }
        });
    }

    fn mouse_dpi_card(&mut self, ui: &mut egui::Ui, width: f32) {
        let can_write = self.mouse_controls_enabled();
        let current = self.mouse_snapshot().map(|snapshot| snapshot.dpi);

        card(ui, width, |ui| {
            section(ui, "DPI");
            if let Some((x, y)) = current {
                ui.label(
                    RichText::new(format!("Current: {x} × {y}"))
                        .size(11.0)
                        .color(MUTED),
                );
            } else {
                ui.label(
                    RichText::new("Current: unavailable")
                        .size(11.0)
                        .color(MUTED),
                );
            }

            ui.add_enabled_ui(can_write, |ui| {
                let link_changed = ui
                    .checkbox(&mut self.mouse_dpi_link, "Link X and Y")
                    .changed();
                if link_changed && self.mouse_dpi_link {
                    self.mouse_dpi_y = self.mouse_dpi_x;
                }

                ui.horizontal(|ui| {
                    ui.label("X");
                    ui.add(
                        egui::DragValue::new(&mut self.mouse_dpi_x)
                            .range(100..=8500)
                            .speed(10.0),
                    );
                    ui.label("Y");
                    ui.add_enabled_ui(!self.mouse_dpi_link, |ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.mouse_dpi_y)
                                .range(100..=8500)
                                .speed(10.0),
                        );
                    });
                });
                if self.mouse_dpi_link {
                    self.mouse_dpi_y = self.mouse_dpi_x;
                }
            });
            ui.label(
                RichText::new("Requests onboard storage. Unplug/replug persistence is untested.")
                    .size(10.5)
                    .color(WARN),
            );

            let valid = (100..=8500).contains(&self.mouse_dpi_x)
                && (100..=8500).contains(&self.mouse_dpi_y);
            let changed =
                current.is_some_and(|value| value != (self.mouse_dpi_x, self.mouse_dpi_y));
            let apply = ui
                .add_enabled(can_write && valid && changed, egui::Button::new("Apply"))
                .clicked();
            if apply {
                self.mouse.set_dpi(self.mouse_dpi_x, self.mouse_dpi_y);
                self.mouse_busy = Some("DPI".to_string());
            }
        });
    }

    fn mouse_logo_card(&mut self, ui: &mut egui::Ui, width: f32) {
        let can_write = self.mouse_controls_enabled();
        let current = self.mouse_snapshot().map(|snapshot| snapshot.brightness);
        let mut open_static = false;

        card(ui, width, |ui| {
            section(ui, "LOGO");
            if let Some(value) = current {
                ui.label(
                    RichText::new(format!("Current brightness: {value}/255"))
                        .size(11.0)
                        .color(MUTED),
                );
            } else {
                ui.label(
                    RichText::new("Current: unavailable")
                        .size(11.0)
                        .color(MUTED),
                );
            }

            ui.add_enabled_ui(can_write, |ui| {
                ui.add(
                    egui::Slider::new(&mut self.mouse_brightness, 0..=255)
                        .text("Brightness")
                        .show_value(true),
                );
            });
            let changed = current.is_some_and(|value| value != self.mouse_brightness);
            let apply_brightness = ui
                .add_enabled(can_write && changed, egui::Button::new("Apply brightness"))
                .clicked();
            if apply_brightness {
                self.mouse.set_brightness(self.mouse_brightness);
                self.mouse_busy = Some("Brightness".to_string());
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let mut picked = self.mouse_static_color.to_color32();
                if ui.color_edit_button_srgba(&mut picked).changed() {
                    self.mouse_static_color = Rgb::from_color32(picked);
                }
                ui.label(
                    RichText::new(self.mouse_static_color.to_hex())
                        .monospace()
                        .size(11.0),
                );
            });
            open_static = ui
                .add_enabled(can_write, egui::Button::new("Set static colour"))
                .clicked();
            ui.label(
                RichText::new("Static colour has no readback and cannot be rolled back.")
                    .size(10.5)
                    .color(WARN),
            );
        });

        if open_static {
            self.mouse_pending_static = Some(self.mouse_static_color);
            self.mouse_static_ack = false;
        }
    }

    fn mouse_details_card(&self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        card(ui, width, |ui| {
            section(ui, "DEVICE DETAILS");
            if let Some(snapshot) = self.mouse_snapshot() {
                let polling = match snapshot.polling_hz {
                    Some(hz) => format!("{hz} Hz (byte 0x{:02X})", snapshot.polling_raw),
                    None => format!("Unknown byte 0x{:02X}", snapshot.polling_raw),
                };
                let stages = snapshot
                    .dpi_stages
                    .stages
                    .iter()
                    .map(|(x, y)| {
                        if x == y {
                            x.to_string()
                        } else {
                            format!("{x} × {y}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" / ");

                egui::Grid::new("mouse_details_grid")
                    .num_columns(2)
                    .spacing([18.0, 5.0])
                    .show(ui, |ui| {
                        mouse_detail_row(
                            ui,
                            "Firmware",
                            &format!("v{}.{}", snapshot.firmware.0, snapshot.firmware.1),
                        );
                        mouse_detail_row(ui, "Serial", &snapshot.serial);
                        mouse_detail_row(
                            ui,
                            "Mode",
                            &format!(
                                "0x{:02X} ({})",
                                snapshot.mode,
                                razer::mode_name(snapshot.mode)
                            ),
                        );
                        mouse_detail_row(ui, "Polling", &polling);
                        mouse_detail_row(
                            ui,
                            "DPI",
                            &format!("{} × {}", snapshot.dpi.0, snapshot.dpi.1),
                        );
                        mouse_detail_row(
                            ui,
                            "DPI stages",
                            &format!(
                                "active {}, {} configured · {}",
                                snapshot.dpi_stages.active_stage,
                                snapshot.dpi_stages.count,
                                if stages.is_empty() { "none" } else { &stages }
                            ),
                        );
                        mouse_detail_row(ui, "Brightness", &format!("{}/255", snapshot.brightness));
                        ui.label(RichText::new("Path").size(11.0).color(MUTED));
                        ui.label(RichText::new(short_razer_path(&snapshot.path)).monospace())
                            .on_hover_text(&snapshot.path);
                        ui.end_row();
                    });
            } else {
                ui.label(
                    RichText::new("Connect the mouse to read its settings.")
                        .size(12.0)
                        .color(MUTED),
                );
            }
        });
    }

    fn mouse_log_card(&self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        card(ui, width, |ui| {
            section(ui, "RECENT OPERATIONS");
            if self.mouse_log.is_empty() {
                ui.label(
                    RichText::new("No mouse operations yet.")
                        .size(12.0)
                        .color(MUTED),
                );
            } else {
                for entry in &self.mouse_log {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("●")
                                .color(if entry.ok { OK } else { BAD })
                                .size(11.0),
                        );
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&entry.label).strong().size(12.0));
                            ui.label(
                                RichText::new(truncated_first_line(&entry.detail, 140))
                                    .monospace()
                                    .size(10.5)
                                    .color(MUTED),
                            )
                            .on_hover_text(&entry.detail);
                        });
                    });
                }
            }
        });
    }

    fn mouse_snapshot(&self) -> Option<&MouseSnapshot> {
        match &self.mouse_status {
            MouseStatus::Connected(snapshot) => Some(snapshot),
            MouseStatus::Connecting | MouseStatus::Unavailable(_) => None,
        }
    }

    fn mouse_can_write(&self) -> bool {
        matches!(&self.mouse_status, MouseStatus::Connected(_)) && self.mouse_busy.is_none()
    }

    fn mouse_controls_enabled(&self) -> bool {
        self.mouse_can_write() && self.mouse_pending_static.is_none()
    }

    fn static_colour_modal(&mut self, ctx: &egui::Context) {
        let Some(color) = self.mouse_pending_static else {
            return;
        };
        let mut open = true;
        let mut cancel = false;
        let mut write = false;
        let can_write = self.mouse_can_write();

        egui::Window::new("Set static logo colour?")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (swatch, _) =
                        ui.allocate_exact_size(Vec2::new(44.0, 28.0), Sense::hover());
                    ui.painter()
                        .rect_filled(swatch, CornerRadius::same(5), color.to_color32());
                    ui.painter().rect_stroke(
                        swatch,
                        CornerRadius::same(5),
                        Stroke::new(1.0, KEY_EDGE),
                        StrokeKind::Inside,
                    );
                    ui.label(RichText::new(color.to_hex()).monospace().strong());
                });
                ui.add_space(8.0);
                ui.label(
                    "This is written directly to the mouse. There is no colour readback, so the colour cannot be verified or restored to its previous value.",
                );
                ui.label(
                    RichText::new(
                        "Brightness is checked before and after the write, but the colour itself is not verifiable.",
                    )
                    .color(WARN),
                );
                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.mouse_static_ack,
                    "I understand the colour cannot be verified or rolled back.",
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    cancel = ui.button("Cancel").clicked();
                    write = ui
                        .add_enabled(
                            self.mouse_static_ack && can_write,
                            egui::Button::new("Write colour"),
                        )
                        .clicked();
                });
            });

        if write {
            self.mouse.set_static(color, true);
            self.mouse_busy = Some("Static colour".to_string());
            self.mouse_pending_static = None;
            self.mouse_static_ack = false;
        } else if cancel || !open {
            self.mouse_pending_static = None;
            self.mouse_static_ack = false;
        }
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        if self.startup_status.is_none() {
            self.startup_status = Some(startup::status());
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(8.0);
            let width = ui.available_width();
            if let Some(error) = &self.save_error {
                card(ui, width, |ui| {
                    section(ui, "STATE FILE");
                    ui.label(
                        RichText::new("Lighting state could not be saved. Changes stay in memory and saving retries.")
                            .size(12.0)
                            .color(BAD),
                    );
                    ui.label(RichText::new(error).monospace().size(11.0).color(BAD));
                });
                ui.add_space(10.0);
            }
            card(ui, width, |ui| {
                section(ui, "STARTUP");
                let mut enabled = matches!(self.startup_status, Some(startup::Status::Enabled { .. }));
                ui.horizontal_wrapped(|ui| {
                    if ui.checkbox(&mut enabled, "Start rgbd when I sign in").changed() {
                        let result = if enabled { startup::enable() } else { startup::disable() };
                        self.startup_error = result.err().map(|error| format!("{error:#}"));
                        self.startup_status = Some(startup::status());
                    }
                    if ui.button("Refresh").clicked() {
                        self.startup_status = Some(startup::status());
                        self.startup_error = None;
                    }
                });
                match std::env::current_exe() {
                    Ok(path) => {
                        ui.label(RichText::new(format!("Current executable: {}", path.display())).monospace().size(12.0));
                    }
                    Err(error) => {
                        ui.label(RichText::new(format!("Could not locate executable: {error}")).color(BAD));
                    }
                }
                match &self.startup_status {
                    Some(startup::Status::Enabled { command }) => {
                        ui.label(RichText::new(format!("Registered command: {command}")).monospace().size(12.0));
                    }
                    Some(startup::Status::Disabled) => {
                        ui.label(RichText::new("No startup entry registered.").size(12.0).color(MUTED));
                    }
                    Some(startup::Status::Error(error)) => {
                        ui.label(RichText::new(format!("Startup status unavailable: {error}")).color(BAD));
                    }
                    None => {}
                }
                if let Some(error) = &self.startup_error {
                    ui.label(RichText::new(error).color(BAD));
                }
                ui.label(RichText::new("The registered path stays fixed. If you move rgbd, switch startup off and on again to update it.").size(12.0).color(MUTED));
            });
            ui.add_space(10.0);
            card(ui, width, |ui| {
                section(ui, "FOLDERS");
                match std::env::var_os("APPDATA").filter(|path| !path.is_empty()) {
                    Some(appdata) => {
                        let settings = PathBuf::from(appdata).join("rgbd");
                        for (label, path) in [("Open settings folder", settings.clone()), ("Open backups folder", settings.join("backups"))] {
                            if ui.button(label).clicked() {
                                self.folder_error = open_folder(&path).err().map(|error| format!("{error:#}"));
                            }
                            ui.label(RichText::new(path.display().to_string()).monospace().size(12.0).color(MUTED));
                        }
                    }
                    None => {
                        ui.label(RichText::new("APPDATA is unavailable; folder paths could not be resolved.").color(BAD));
                    }
                }
                if let Some(error) = &self.folder_error {
                    ui.label(RichText::new(error).color(BAD));
                }
            });
            ui.add_space(10.0);
            card(ui, width, |ui| {
                section(ui, "ABOUT");
                ui.label(RichText::new(format!("rgbd {}", env!("CARGO_PKG_VERSION"))).size(18.0).strong().color(ACCENT));
                ui.label(RichText::new(format!("License: {}", env!("CARGO_PKG_LICENSE"))).size(12.0).color(MUTED));
                let repository = env!("CARGO_PKG_REPOSITORY");
                if repository.is_empty() {
                    // No upstream URL is configured in the package metadata.
                    let checkout = env!("CARGO_MANIFEST_DIR")
                        .replace('\\', "/")
                        .replace('%', "%25")
                        .replace(' ', "%20")
                        .replace('#', "%23")
                        .replace('?', "%3F");
                    ui.hyperlink_to("Repository (local checkout)", format!("file:///{checkout}"));
                } else {
                    ui.hyperlink_to("Repository", repository);
                }
            });
            ui.add_space(8.0);
        });
    }

    fn diagnostics_tab(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(14.0);
            ui.label(RichText::new("Diagnostics").size(22.0).strong());
            ui.add_space(10.0);

            egui::Grid::new("diag_grid")
                .num_columns(2)
                .spacing([18.0, 6.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Device").color(MUTED));
                    ui.label("AULA F99 (258A:010C)");
                    ui.end_row();

                    ui.label(RichText::new("Transport").color(MUTED));
                    ui.label("HID feature reports, report ID 0x06, 520 bytes");
                    ui.end_row();

                    ui.label(RichText::new("Stream").color(MUTED));
                    ui.label(format!(
                        "0x08, {STREAM_SLOTS} slots, change-only + 1 s keepalive"
                    ));
                    ui.end_row();

                    ui.label(RichText::new("Key LEDs").color(MUTED));
                    ui.label(format!("{} mapped keys", F99_KEYS.len()));
                    ui.end_row();

                    ui.label(RichText::new("Connection").color(MUTED));
                    match &self.status {
                        Status::Connecting => {
                            ui.label("connecting");
                        }
                        Status::Connected { .. } => {
                            ui.label(RichText::new("connected").color(OK));
                        }
                        Status::Unavailable(_) => {
                            ui.label(RichText::new("unavailable").color(BAD));
                        }
                    }
                    ui.end_row();

                    ui.label(RichText::new("Contention").color(MUTED));
                    if self.conflicts.is_empty() {
                        ui.label(RichText::new("none detected").color(OK));
                    } else {
                        let detail = self
                            .conflicts
                            .iter()
                            .map(|conflict| {
                                format!("{} (also running)", conflict.label)
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        ui.label(RichText::new(detail).color(BAD));
                    }
                    ui.end_row();

                    if let Status::Connected {
                        path,
                        model_id,
                        mode_bytes,
                    } = &self.status
                    {
                        ui.label(RichText::new("Path").color(MUTED));
                        ui.label(RichText::new(short_path(path)).monospace());
                        ui.end_row();

                        ui.label(RichText::new("Model ID").color(MUTED));
                        ui.label(match model_id {
                            Some(id) => format!("0x{id:02X}"),
                            None => "not reported".to_string(),
                        });
                        ui.end_row();

                        ui.label(RichText::new("Mode bytes").color(MUTED));
                        ui.label(match mode_bytes {
                            Some(mode) => {
                                let mode_name = if *mode == MODE_PER_KEY {
                                    "per-key"
                                } else {
                                    "firmware effect"
                                };
                                format!(
                                    "{:02X} {:02X} {:02X} ({mode_name})",
                                    mode[0], mode[1], mode[2]
                                )
                            }
                            None => "not reported".to_string(),
                        });
                        ui.end_row();
                    }
                });

            ui.add_space(16.0);
            ui.separator();
            ui.add_space(10.0);
            ui.label(RichText::new("Safety").size(14.0).strong());
            ui.add_space(4.0);
            for line in [
                "Only the volatile 0x08 streaming path is written during animation.",
                "Static frames are sent on change, then refreshed every second: 0x08 is volatile.",
                "Close SignalRGB, OpenRGB, and the AULA software; two writers alternate frames and flicker.",
                "Config writes (0x04) are limited to a guarded mode switch in `rgbctl`.",
                "Mode writes read twice, back up, patch only bytes 9/10/59, and verify the readback.",
                "The direct 0x06 path is avoided: repeated writes can blank the board.",
            ] {
                ui.label(RichText::new(format!("•  {line}")).size(12.0).color(MUTED));
            }

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Copy diagnostics").clicked() {
                    ui.ctx().copy_text(self.diagnostics_text());
                    self.diagnostics_copied = Some(Instant::now());
                }
                if self.diagnostics_copied.is_some_and(|copied| copied.elapsed() < Duration::from_secs(2)) {
                    ui.label(RichText::new("Copied").size(12.0).color(OK));
                    ui.ctx().request_repaint_after(Duration::from_secs(2));
                }
            });
        });
    }

    fn diagnostics_text(&self) -> String {
        let mut out = String::new();
        out.push_str("rgbd diagnostics\n");
        out.push_str("device: AULA F99 (258A:010C)\n");
        out.push_str(&format!("keys: {}\n", F99_KEYS.len()));
        out.push_str(&format!("stream slots: {STREAM_SLOTS}\n"));
        out.push_str("stream cadence: change-only + 1 s keepalive\n");
        if self.conflicts.is_empty() {
            out.push_str("contention: none detected\n");
        } else {
            for conflict in &self.conflicts {
                out.push_str(&format!(
                    "contention: {} ({})\n",
                    conflict.label,
                    conflict.processes.join(", ")
                ));
            }
        }
        match &self.status {
            Status::Connecting => out.push_str("connection: connecting\n"),
            Status::Connected {
                path,
                model_id,
                mode_bytes,
            } => {
                out.push_str("connection: connected\n");
                out.push_str(&format!("path: {path}\n"));
                if let Some(id) = model_id {
                    out.push_str(&format!("model: 0x{id:02X}\n"));
                }
                if let Some(mode) = mode_bytes {
                    out.push_str(&format!(
                        "mode: {:02X} {:02X} {:02X}\n",
                        mode[0], mode[1], mode[2]
                    ));
                }
            }
            Status::Unavailable(err) => {
                out.push_str(&format!("connection: unavailable\n{err}\n"));
            }
        }
        out
    }
}

impl eframe::App for RgbdApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(conflicts) = self.conflict_monitor.poll() {
            self.conflicts = conflicts;
        }
        while let Some(status) = self.engine.poll_status() {
            self.status = status;
        }
        while let Some(event) = self.mouse.poll_event() {
            match event {
                MouseEvent::Status(status) => {
                    let same_connection = match (&self.mouse_status, &status) {
                        (MouseStatus::Connected(previous), MouseStatus::Connected(next)) => {
                            previous.path == next.path
                        }
                        _ => false,
                    };
                    if !same_connection {
                        self.mouse_static_ack = false;
                    }
                    if let MouseStatus::Connected(snapshot) = &status {
                        if snapshot.dpi.0 != snapshot.dpi.1 {
                            self.mouse_dpi_link = false;
                        }
                        self.mouse_poll = snapshot.polling_hz.unwrap_or(self.mouse_poll);
                        self.mouse_dpi_x = snapshot.dpi.0;
                        self.mouse_dpi_y = snapshot.dpi.1;
                        self.mouse_brightness = snapshot.brightness;
                    }
                    self.mouse_status = status;
                }
                MouseEvent::Busy(label) => self.mouse_busy = label,
                MouseEvent::Result { label, detail, ok } => {
                    self.mouse_log.push(MouseLogEntry { ok, label, detail });
                    if self.mouse_log.len() > 6 {
                        self.mouse_log.remove(0);
                    }
                }
            }
        }

        let ctx = ui.ctx().clone();
        self.top_bar(ui);

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            match self.tab {
                Tab::Lighting => self.lighting_tab(ui),
                Tab::Remap => self.remap_tab(ui),
                Tab::Mouse => self.mouse_tab(ui),
                Tab::Settings => self.settings_tab(ui),
                Tab::Diagnostics => self.diagnostics_tab(ui),
            }
        });

        self.static_colour_modal(&ctx);
        self.push_frame(&ctx);
        self.save_if_due();
        if self.mouse_busy.is_some() || matches!(&self.mouse_status, MouseStatus::Connecting) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

impl Drop for RgbdApp {
    fn drop(&mut self) {
        // Persist before joining the worker: an in-flight HID call can delay
        // shutdown, but must never cost the user's saved lighting state.
        self.save_now();
        self.engine.stop();
        self.mouse.stop();
    }
}

fn open_folder(path: &Path) -> anyhow::Result<()> {
    use anyhow::Context;

    fs::create_dir_all(path).with_context(|| format!("Could not create {}", path.display()))?;
    let explorer = std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("explorer.exe"))
        .context("SystemRoot is not set")?;
    Command::new(explorer)
        .arg(path)
        .creation_flags(0x08000000)
        .spawn()
        .with_context(|| format!("Could not open {} in Explorer", path.display()))?;
    Ok(())
}

fn configure_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = Color32::from_rgb(21, 24, 30);
    visuals.extreme_bg_color = Color32::from_rgb(8, 10, 13);
    visuals.faint_bg_color = Color32::from_rgb(24, 27, 33);
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.30);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, KEY_EDGE);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(28, 32, 39);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(24, 27, 33);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(38, 43, 52);
    visuals.widgets.active.bg_fill = Color32::from_rgb(46, 52, 62);
    ctx.set_visuals_of(egui::Theme::Dark, visuals);

    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(10.0, 6.0);
        style.spacing.slider_width = 150.0;
    });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(10.0).color(MUTED).strong());
    ui.add_space(3.0);
}

fn card<R>(ui: &mut egui::Ui, width: f32, add_contents: impl FnOnce(&mut egui::Ui) -> R) {
    egui::Frame::new()
        .fill(Color32::from_rgb(14, 17, 21))
        .stroke(Stroke::new(1.0, KEY_EDGE))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            let content_width = (width - 28.0).max(120.0);
            ui.set_width(content_width);
            ui.vertical(|ui| {
                ui.set_width(content_width);
                ui.set_min_height(124.0);
                add_contents(ui);
            });
        });
}

fn key_rect(origin: Pos2, scale: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> Rect {
    Rect::from_min_max(
        Pos2::new(origin.x + x1 * scale, origin.y + y1 * scale),
        Pos2::new(origin.x + x2 * scale, origin.y + y2 * scale),
    )
}

fn draw_key(
    painter: &egui::Painter,
    rect: Rect,
    label: &str,
    color: Rgb,
    hovered: bool,
    scale: f32,
) {
    let fill = color.to_color32();
    let radius = CornerRadius::same((3.0 + scale * 1.5).clamp(2.0, 6.0) as u8);
    painter.rect_filled(rect, radius, fill);

    let edge = if hovered {
        Stroke::new((1.2 * scale).clamp(1.0, 2.0), ACCENT)
    } else {
        Stroke::new(1.0, KEY_EDGE)
    };
    painter.rect_stroke(rect, radius, edge, StrokeKind::Inside);

    let (r, g, b) = (u32::from(color.r), u32::from(color.g), u32::from(color.b));
    let luma = (r * 299 + g * 587 + b * 114) / 1000;
    let text_color = if luma > 138 {
        Color32::from_rgb(12, 14, 18)
    } else {
        Color32::from_rgb(196, 203, 214)
    };

    let size = if label.len() > 4 {
        (rect.height() * 0.26).clamp(5.5, 9.5)
    } else if label.len() > 2 {
        (rect.height() * 0.30).clamp(6.0, 11.0)
    } else {
        (rect.height() * 0.36).clamp(7.0, 13.0)
    };

    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(size),
        text_color,
    );
}

fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> Rgb {
    let sector = (hue.rem_euclid(1.0) * 6.0).floor();
    let fraction = hue.rem_euclid(1.0) * 6.0 - sector;
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * fraction);
    let t = value * (1.0 - saturation * (1.0 - fraction));

    let (r, g, b) = match sector as i32 % 6 {
        0 => (value, t, p),
        1 => (q, value, p),
        2 => (p, value, t),
        3 => (p, q, value),
        4 => (t, p, value),
        _ => (value, p, q),
    };

    Rgb::new(to_byte(r), to_byte(g), to_byte(b))
}

fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn short_path(path: &str) -> String {
    path.rsplit_once("MI_01&")
        .map(|(_, rest)| format!("MI_01&{}", rest.split('#').next().unwrap_or(rest)))
        .unwrap_or_else(|| path.to_string())
}

fn short_razer_path(path: &str) -> String {
    path.rsplit_once("MI_00#")
        .map(|(_, rest)| format!("MI_00#{}", rest.split('#').next().unwrap_or(rest)))
        .unwrap_or_else(|| path.to_string())
}

fn mouse_detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).size(11.0).color(MUTED));
    ui.label(RichText::new(value).size(11.0));
    ui.end_row();
}

fn truncated_first_line(text: &str, max_chars: usize) -> String {
    let line = first_line(text);
    let mut chars = line.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).to_string()
}

impl Rgb {
    fn to_color32(self) -> Color32 {
        Color32::from_rgb(self.r, self.g, self.b)
    }

    fn from_color32(color: Color32) -> Self {
        Self::new(color.r(), color.g(), color.b())
    }

    fn scaled(self, factor: f32) -> Self {
        Self::new(
            to_byte(f32::from(self.r) / 255.0 * factor),
            to_byte(f32::from(self.g) / 255.0 * factor),
            to_byte(f32::from(self.b) / 255.0 * factor),
        )
    }
}

const PALETTE: [Rgb; 10] = [
    Rgb::new(255, 59, 48),
    Rgb::new(255, 149, 0),
    Rgb::new(255, 214, 10),
    Rgb::new(52, 199, 89),
    Rgb::new(0, 229, 255),
    Rgb::new(10, 132, 255),
    Rgb::new(94, 92, 230),
    Rgb::new(255, 45, 146),
    Rgb::new(255, 255, 255),
    Rgb::BLACK,
];

/// Small INI-style file that survives restarts without pulling in serde.
struct Persisted {
    colors: StreamFrame,
    color: Rgb,
    brightness: u8,
    effect: Effect,
    speed: f32,
}

impl Persisted {
    fn load() -> Option<Self> {
        let path = state_path();
        let text = fs::read_to_string(&path).ok()?;

        let mut colors = vec![Rgb::BLACK; STREAM_SLOTS];
        let mut color = Rgb::new(0, 229, 255);
        let mut brightness = 220;
        let mut effect = Effect::Static;
        let mut speed = 1.0;

        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "frame" => {
                    if let Some(frame) = decode_frame(value.trim()) {
                        colors = frame;
                    }
                }
                "color" => {
                    if let Ok(parsed) = Rgb::from_hex(value.trim()) {
                        color = parsed;
                    }
                }
                "brightness" => {
                    brightness = value.trim().parse().unwrap_or(220);
                }
                "effect" => {
                    effect = Effect::from_key(value.trim()).unwrap_or(Effect::Static);
                }
                "speed" => {
                    let parsed: f32 = value.trim().parse().unwrap_or(1.0);
                    speed = parsed.clamp(0.2, 2.5);
                }
                _ => {}
            }
        }

        Some(Self {
            colors,
            color,
            brightness,
            effect,
            speed,
        })
    }

    fn save(&self) -> std::io::Result<()> {
        let path = state_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let frame: String = self
            .colors
            .iter()
            .map(|c| format!("{:02x}{:02x}{:02x}", c.r, c.g, c.b))
            .collect();

        let text = format!(
            "version=1\ncolor={}\nbrightness={}\neffect={}\nspeed={:.2}\nframe={}\n",
            self.color.to_hex().trim_start_matches('#'),
            self.brightness,
            self.effect.key(),
            self.speed,
            frame
        );
        // Write to a sibling file first. Renaming over the old state keeps a
        // failed write from destroying the last good frame.
        let temporary = path.with_file_name("state.ini.tmp");
        fs::write(&temporary, text)?;
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(())
    }
}

fn decode_frame(hex: &str) -> Option<StreamFrame> {
    if hex.len() != STREAM_SLOTS * 6 {
        return None;
    }
    let bytes = hex.as_bytes();
    let mut frame = Vec::with_capacity(STREAM_SLOTS);
    for chunk in bytes.as_chunks::<6>().0 {
        let r = u8::from_str_radix(std::str::from_utf8(&chunk[0..2]).ok()?, 16).ok()?;
        let g = u8::from_str_radix(std::str::from_utf8(&chunk[2..4]).ok()?, 16).ok()?;
        let b = u8::from_str_radix(std::str::from_utf8(&chunk[4..6]).ok()?, 16).ok()?;
        frame.push(Rgb::new(r, g, b));
    }
    Some(frame)
}

fn state_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rgbd")
        .join("state.ini")
}
