mod app;
mod events;
mod hotkeys;
mod i18n;
mod input_backend;
#[cfg(all(target_os = "linux", feature = "portal-hotkeys"))]
mod portal_hotkeys;
mod screen_guard;
mod settings_panel;
mod step_editor;
mod tray;

fn main() -> eframe::Result {
    tracing_subscriber::fmt::init();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([460.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Macro Recorder",
        native_options,
        Box::new(|_cc| Ok(Box::new(app::MacroRecorderApp::new()))),
    )
}
