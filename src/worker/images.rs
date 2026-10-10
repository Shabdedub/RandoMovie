//! Image decoding on the worker: decode, scale to the exact physical pixels shown, round the
//! corners (the software renderer can't clip images to rounded rectangles), and hand back a
//! `SharedPixelBuffer` for the UI thread to wrap in a `slint::Image`.

use image::RgbaImage;
use image::imageops::FilterType;
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Corner radius as a fraction of the logo's size.
const LOGO_RADIUS: f32 = 0.22;

pub fn decode_logo(bytes: &[u8], px: u32) -> Result<SharedPixelBuffer<Rgba8Pixel>, String> {
    let px = px.clamp(8, 1024);
    let decoded = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let mut rgba = decoded
        .resize_to_fill(px, px, FilterType::CatmullRom)
        .into_rgba8();
    round_corners(&mut rgba, px as f32 * LOGO_RADIUS);
    Ok(SharedPixelBuffer::clone_from_slice(rgba.as_raw(), px, px))
}

/// Makes the corners outside a circle of `radius` transparent, with one pixel of
/// anti-aliasing along the curve.
pub fn round_corners(image: &mut RgbaImage, radius: f32) {
    let (width, height) = image.dimensions();
    let (w, h) = (width as f32, height as f32);
    for y in 0..height {
        let cy = y as f32 + 0.5;
        let dy = (radius - cy).max(cy - (h - radius)).max(0.0);
        if dy == 0.0 {
            continue;
        }
        for x in 0..width {
            let cx = x as f32 + 0.5;
            let dx = (radius - cx).max(cx - (w - radius)).max(0.0);
            if dx == 0.0 {
                continue;
            }
            let coverage = (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            if coverage < 1.0 {
                let pixel = image.get_pixel_mut(x, y);
                pixel[3] = (f32::from(pixel[3]) * coverage).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba};
    use std::io::Cursor;

    #[test]
    fn rounds_corners_and_keeps_the_middle() {
        let mut img = RgbaImage::from_pixel(40, 40, Rgba([10, 20, 30, 255]));
        round_corners(&mut img, 10.0);
        assert_eq!(img.get_pixel(0, 0)[3], 0, "corner is transparent");
        assert_eq!(img.get_pixel(39, 39)[3], 0);
        assert_eq!(img.get_pixel(20, 20)[3], 255, "centre untouched");
        assert_eq!(img.get_pixel(20, 0)[3], 255, "straight edge untouched");
        assert_eq!(img.get_pixel(0, 20)[3], 255);
        let curve = img.get_pixel(2, 2)[3];
        assert!(curve < 255, "pixels on the curve are partly transparent");
    }

    #[test]
    fn decodes_png_to_exact_size() {
        let source = RgbaImage::from_pixel(92, 92, Rgba([200, 30, 40, 255]));
        let mut png = Vec::new();
        source.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        let buffer = decode_logo(&png, 66).unwrap();
        assert_eq!((buffer.width(), buffer.height()), (66, 66));
        let centre = buffer.as_slice()[33 * 66 + 33];
        assert_eq!((centre.r, centre.g, centre.b, centre.a), (200, 30, 40, 255));
        assert_eq!(buffer.as_slice()[0].a, 0);
        assert!(decode_logo(b"not an image", 32).is_err());
    }
}
