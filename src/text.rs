//! Text on a pixmap, from the embedded fonts.
//!
//! `Fonts` holds the three faces the display uses: Inter Bold for
//! values, Inter Medium for labels, and the six-letter wordmark
//! face. `draw` rasterizes a string at a pixel size with fontdue
//! and blends it onto an opaque pixmap.

use tiny_skia::Pixmap;

/// A color as red, green, blue.
pub type Rgb = [u8; 3];

/// How a run of text is set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub face: Face,
    pub px: f32,
    pub color: Rgb,
}

const INTER_BOLD: &[u8] = include_bytes!("../assets/Inter-Bold.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../assets/Inter-Medium.ttf");
const WORDMARK: &[u8] = include_bytes!("../assets/bridge-officer.ttf");

/// Which face to set text in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    Bold,
    Medium,
    /// Bridge Officer, subset to the letters of "mujina".
    Wordmark,
}

/// The embedded faces, parsed once.
pub struct Fonts {
    bold: fontdue::Font,
    medium: fontdue::Font,
    wordmark: fontdue::Font,
}

impl Fonts {
    pub fn load() -> Self {
        let parse = |bytes| {
            fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
                .expect("the embedded font parses")
        };
        Self {
            bold: parse(INTER_BOLD),
            medium: parse(INTER_MEDIUM),
            wordmark: parse(WORDMARK),
        }
    }

    fn face(&self, face: Face) -> &fontdue::Font {
        match face {
            Face::Bold => &self.bold,
            Face::Medium => &self.medium,
            Face::Wordmark => &self.wordmark,
        }
    }

    /// The advance width of `text` at `px`, in pixels.
    pub fn measure(&self, face: Face, text: &str, px: f32) -> f32 {
        let font = self.face(face);
        text.chars()
            .map(|ch| font.metrics(ch, px).advance_width)
            .sum()
    }

    /// The distance from the top of the line to the baseline at
    /// `px`, which is where a glyph without a descender ends.
    pub fn ascent(&self, face: Face, px: f32) -> f32 {
        self.face(face)
            .horizontal_line_metrics(px)
            .map(|m| m.ascent)
            .unwrap_or(px)
    }

    /// Draws `text` with its top-left corner at (`x`, `y`).
    pub fn draw(&self, pixmap: &mut Pixmap, style: Style, x: i32, y: i32, text: &str) {
        let Style { face, px, color } = style;
        let font = self.face(face);
        let ascent = self.ascent(face, px);
        let width = pixmap.width() as i32;
        let height = pixmap.height() as i32;
        let stride = width as usize * 4;
        let data = pixmap.data_mut();

        let mut pen = x as f32;
        for ch in text.chars() {
            let (metrics, bitmap) = font.rasterize(ch, px);
            let left = pen as i32 + metrics.xmin;
            let top = y + ascent as i32 - metrics.height as i32 - metrics.ymin;
            for row in 0..metrics.height {
                let py = top + row as i32;
                if py < 0 || py >= height {
                    continue;
                }
                for col in 0..metrics.width {
                    let coverage = bitmap[row * metrics.width + col] as u16;
                    if coverage == 0 {
                        continue;
                    }
                    let px_x = left + col as i32;
                    if px_x < 0 || px_x >= width {
                        continue;
                    }
                    let i = py as usize * stride + px_x as usize * 4;
                    let inv = 255 - coverage;
                    for (c, &value) in color.iter().enumerate() {
                        data[i + c] =
                            ((value as u16 * coverage + data[i + c] as u16 * inv) / 255) as u8;
                    }
                    data[i + 3] = 255;
                }
            }
            pen += metrics.advance_width;
        }
    }

    /// Draws `text` with its top-right corner at (`right`, `y`).
    pub fn draw_right(&self, pixmap: &mut Pixmap, style: Style, right: i32, y: i32, text: &str) {
        let width = self.measure(style.face, text, style.px);
        self.draw(pixmap, style, right - width as i32, y, text);
    }

    /// Draws `text` centered on `cx` with its top at `y`.
    pub fn draw_centered(&self, pixmap: &mut Pixmap, style: Style, cx: i32, y: i32, text: &str) {
        let width = self.measure(style.face, text, style.px);
        self.draw(pixmap, style, cx - (width / 2.0) as i32, y, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(face: Face, px: f32) -> Style {
        Style {
            face,
            px,
            color: [255, 255, 255],
        }
    }

    fn lit_pixels(pixmap: &Pixmap) -> usize {
        pixmap
            .data()
            .chunks_exact(4)
            .filter(|px| px[0] > 0 || px[1] > 0 || px[2] > 0)
            .count()
    }

    #[test]
    fn draws_something_and_more_at_a_larger_size() {
        let fonts = Fonts::load();
        let mut small = Pixmap::new(400, 120).unwrap();
        fonts.draw(&mut small, white(Face::Bold, 24.0), 0, 0, "8");
        let mut large = Pixmap::new(400, 120).unwrap();
        fonts.draw(&mut large, white(Face::Bold, 72.0), 0, 0, "8");
        assert!(lit_pixels(&small) > 0);
        assert!(lit_pixels(&large) > lit_pixels(&small));
    }

    #[test]
    fn the_wordmark_face_sets_its_six_letters() {
        let fonts = Fonts::load();
        let mut pixmap = Pixmap::new(400, 120).unwrap();
        fonts.draw(&mut pixmap, white(Face::Wordmark, 48.0), 0, 0, "mujina");
        assert!(lit_pixels(&pixmap) > 0);
        assert!(fonts.measure(Face::Wordmark, "mujina", 48.0) > 0.0);
    }

    #[test]
    fn right_alignment_ends_at_the_edge_given() {
        let fonts = Fonts::load();
        let mut pixmap = Pixmap::new(200, 60).unwrap();
        fonts.draw_right(&mut pixmap, white(Face::Bold, 40.0), 150, 0, "42");
        let lit_columns: Vec<usize> = (0..200)
            .filter(|&x| (0..60).any(|y| pixmap.pixel(x as u32, y).unwrap().red() > 0))
            .collect();
        let right = *lit_columns.last().unwrap();
        assert!((140..=150).contains(&right), "rightmost lit column {right}");
    }

    #[test]
    fn clips_off_the_edges_without_panicking() {
        let fonts = Fonts::load();
        let mut pixmap = Pixmap::new(50, 20).unwrap();
        fonts.draw(
            &mut pixmap,
            white(Face::Medium, 40.0),
            -30,
            -10,
            "clipped text",
        );
        fonts.draw(
            &mut pixmap,
            white(Face::Medium, 40.0),
            45,
            15,
            "clipped text",
        );
    }
}
