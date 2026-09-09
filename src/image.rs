//! Embedded images, decoded from PNG and blitted onto a pixmap.

use tiny_skia::Pixmap;

/// The badger head icon from the Mujina logo set, 295 by 239, the
/// variant drawn for sizes under 80 pixels.
pub const BADGER_HEAD: &[u8] = include_bytes!("../assets/mujina-head-icon.png");

/// A decoded image, straight RGBA.
pub struct Image {
    pub width: u32,
    pub height: u32,
    data: Vec<u8>,
}

impl Image {
    /// Decodes a PNG. Panics on anything but an 8-bit RGB or RGBA
    /// image, since every image here is embedded and known.
    pub fn from_png(bytes: &[u8]) -> Self {
        let decoder = png::Decoder::new(bytes);
        let mut reader = decoder.read_info().expect("PNG header");
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("PNG frame");
        buf.truncate(info.buffer_size());
        let data = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf
                .chunks_exact(3)
                .flat_map(|px| [px[0], px[1], px[2], 255])
                .collect(),
            other => panic!("unsupported PNG color type {other:?}"),
        };
        Self {
            width: info.width,
            height: info.height,
            data,
        }
    }

    /// The width the image has when scaled to `height` pixels tall.
    pub fn width_at(&self, height: u32) -> u32 {
        (self.width as f64 * height as f64 / self.height as f64) as u32
    }

    /// Blits the image with its top-left corner at (`x`, `y`),
    /// scaled to `height` pixels tall with the aspect kept, sampled
    /// nearest-neighbor and blended by its alpha.
    pub fn blit(&self, pixmap: &mut Pixmap, x: i32, y: i32, height: u32) {
        let scale = height as f64 / self.height as f64;
        let width = self.width_at(height);
        let dst_w = pixmap.width() as i32;
        let dst_h = pixmap.height() as i32;
        let dst_stride = dst_w as usize * 4;
        let src_stride = self.width as usize * 4;
        let dst = pixmap.data_mut();
        for ty in 0..height as i32 {
            let py = y + ty;
            if py < 0 || py >= dst_h {
                continue;
            }
            let sy = ((ty as f64 / scale) as usize).min(self.height as usize - 1);
            for tx in 0..width as i32 {
                let px = x + tx;
                if px < 0 || px >= dst_w {
                    continue;
                }
                let sx = ((tx as f64 / scale) as usize).min(self.width as usize - 1);
                let si = sy * src_stride + sx * 4;
                let di = py as usize * dst_stride + px as usize * 4;
                let alpha = self.data[si + 3] as u16;
                if alpha == 0 {
                    continue;
                }
                let inv = 255 - alpha;
                for c in 0..3 {
                    dst[di + c] =
                        ((self.data[si + c] as u16 * alpha + dst[di + c] as u16 * inv) / 255) as u8;
                }
                dst[di + 3] = 255;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_badger_decodes_and_blits() {
        let image = Image::from_png(BADGER_HEAD);
        assert_eq!((image.width, image.height), (295, 239));
        let mut pixmap = Pixmap::new(200, 100).unwrap();
        image.blit(&mut pixmap, 10, 0, 100);
        let lit = pixmap
            .data()
            .chunks_exact(4)
            .filter(|px| px[0] > 0 || px[1] > 0 || px[2] > 0)
            .count();
        assert!(lit > 0);
        assert_eq!(image.width_at(100), 123);
    }

    #[test]
    fn blits_past_the_edges_without_panicking() {
        let image = Image::from_png(BADGER_HEAD);
        let mut pixmap = Pixmap::new(50, 50).unwrap();
        image.blit(&mut pixmap, -40, -40, 300);
    }
}
