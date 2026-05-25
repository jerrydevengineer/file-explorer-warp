mod app;
mod core;
mod git;
mod ui;
mod platform;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("File Explorer")
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "File Explorer",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
