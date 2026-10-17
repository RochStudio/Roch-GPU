//! Embedded images: the 256 px app icon (window icon, title strip) and the multi-size .ico (tray).

use std::sync::Arc;

use eframe::egui::{self, IconData};

pub const ICON_PNG: &[u8] = include_bytes!("../../assets/icon.png");
pub const ICO: &[u8] = include_bytes!("../../assets/RochGPU.ico");

/// Decode a PNG to (rgba, width, height).
pub fn png_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

/// The frame of an .ico closest to `size` px, decoded. Only PNG-compressed frames are read (the
/// shipped icon stores every frame that way).
pub fn ico_frame_rgba(ico: &[u8], size: u32) -> Option<(Vec<u8>, u32, u32)> {
    let u16_at = |o: usize| ico.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| ico.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if u16_at(2)? != 1 {
        return None;
    }
    let count = u16_at(4)? as usize;
    let mut best: Option<(u32, usize, usize)> = None;
    for i in 0..count {
        let e = 6 + i * 16;
        let w = match *ico.get(e)? {
            0 => 256,
            w => w as u32,
        };
        let len = u32_at(e + 8)? as usize;
        let off = u32_at(e + 12)? as usize;
        let diff = w.abs_diff(size);
        if best.is_none_or(|(d, _, _)| diff < d) {
            best = Some((diff, off, len));
        }
    }
    let (_, off, len) = best?;
    let data = ico.get(off..off.checked_add(len)?)?;
    if !data.starts_with(b"\x89PNG") {
        return None;
    }
    png_rgba(data)
}

/// The window icon.
pub fn window_icon() -> Arc<IconData> {
    match png_rgba(ICON_PNG) {
        Some((rgba, width, height)) => Arc::new(IconData { rgba, width, height }),
        None => Arc::new(IconData::default()),
    }
}

/// The icon as a texture for the 24 px dragon in the title strip.
pub fn icon_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let (rgba, w, h) = png_rgba(ICON_PNG)?;
    let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
    Some(ctx.load_texture("roch_icon", image, egui::TextureOptions::LINEAR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icons_decode() {
        let (_, w, h) = png_rgba(ICON_PNG).unwrap();
        assert_eq!((w, h), (256, 256));
        let (rgba, w, h) = ico_frame_rgba(ICO, 16).unwrap();
        assert_eq!((w, h), (16, 16));
        assert_eq!(rgba.len(), 16 * 16 * 4);
    }
}
