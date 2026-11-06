#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Rustyrobo")
        .with_app_id("rustyrobo")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([800.0, 500.0]);
    if let Some(icon) = rustyrobo::assets::logo_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("rustyrobo", options, Box::new(|cc| {
        let app = rustyrobo::app::App::new(cc);
        // Here and not in `App::new`: tests must not reach the network.
        if app.autotest.is_none() && app.settings.check_updates {
            app.check_for_update();
        }
        Ok(Box::new(app))
    }))
}
