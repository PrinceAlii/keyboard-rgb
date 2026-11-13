//! `rgbd` — native lighting control for the AULA F99.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use rgbd::app::RgbdApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([1000.0, 620.0])
            .with_title("rgbd — AULA F99"),
        ..Default::default()
    };

    eframe::run_native(
        "rgbd",
        options,
        Box::new(|cc| Ok(Box::new(RgbdApp::new(cc)))),
    )
}
