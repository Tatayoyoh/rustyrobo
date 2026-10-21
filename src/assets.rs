//! Embedded images.

/// Application logo, 256×256 RGBA (rendered from `docs/logo.svg`).
pub const LOGO_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

/// Decodes an RGBA PNG into `(pixels, width, height)`.
pub fn decode_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    (info.color_type == png::ColorType::Rgba).then_some((buf, info.width, info.height))
}

pub fn logo_icon() -> Option<egui::IconData> {
    let (rgba, width, height) = decode_png(LOGO_PNG)?;
    Some(egui::IconData { rgba, width, height })
}

pub fn logo_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let (rgba, w, h) = decode_png(LOGO_PNG)?;
    let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
    Some(ctx.load_texture("rustyrobo-logo", image, egui::TextureOptions::LINEAR))
}

#[cfg(test)]
mod tests {
    #[test]
    fn logo_decodes() {
        let (px, w, h) = super::decode_png(super::LOGO_PNG).expect("RGBA png");
        assert_eq!((w, h), (256, 256));
        assert_eq!(px.len(), 256 * 256 * 4);
    }
}
