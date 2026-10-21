#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn icon() -> Option<egui::IconData> {
    let decoder = png::Decoder::new(std::io::Cursor::new(include_bytes!("../assets/icon-256.png")));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    (info.color_type == png::ColorType::Rgba).then_some(egui::IconData { rgba: buf, width: info.width, height: info.height })
}

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("rustyrobo")
        .with_app_id("rustyrobo")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([800.0, 500.0]);
    if let Some(icon) = icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("rustyrobo", options, Box::new(|cc| Ok(Box::new(rustyrobo::app::App::new(cc)))))
}
