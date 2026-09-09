//! Where a rendered frame goes: the Linux framebuffer, a PNG file,
//! or nowhere.
//!
//! `Output` is the choice, parsed from the command line. `open`
//! turns it into a `Sink` and the frame size to render at. The
//! framebuffer's size and pixel format come from sysfs, so the
//! same binary drives whatever panel is attached.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use tiny_skia::Pixmap;

/// Where frames go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// A framebuffer device such as `/dev/fb0`.
    Framebuffer(PathBuf),
    /// A PNG file, rewritten with every frame.
    Png(PathBuf),
    /// Nowhere, for a headless run.
    None,
}

impl FromStr for Output {
    type Err = String;

    /// `none`, a path ending in `.png`, or a device path.
    fn from_str(s: &str) -> Result<Self, String> {
        if s == "none" {
            Ok(Output::None)
        } else if s.ends_with(".png") {
            Ok(Output::Png(PathBuf::from(s)))
        } else if s.starts_with('/') {
            Ok(Output::Framebuffer(PathBuf::from(s)))
        } else {
            Err(format!("{s}: expected none, a .png path, or a device path"))
        }
    }
}

/// A frame size, parsed from `WIDTHxHEIGHT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl FromStr for Size {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (w, h) = s
            .split_once('x')
            .ok_or_else(|| format!("{s}: expected WIDTHxHEIGHT"))?;
        let parse = |v: &str| v.parse::<u32>().map_err(|e| format!("{s}: {e}"));
        let size = Size {
            width: parse(w)?,
            height: parse(h)?,
        };
        if size.width == 0 || size.height == 0 {
            return Err(format!("{s}: a side is zero"));
        }
        Ok(size)
    }
}

/// Something a frame can be presented to.
pub trait Sink: Send {
    fn present(&mut self, frame: &Pixmap) -> Result<()>;
}

/// Opens the output. The framebuffer dictates its own size; the
/// others take `fallback`.
pub fn open(output: &Output, fallback: Size) -> Result<(Box<dyn Sink>, Size)> {
    match output {
        Output::Framebuffer(path) => {
            let fb = Framebuffer::open(path)?;
            let size = fb.size();
            Ok((Box::new(fb), size))
        }
        Output::Png(path) => Ok((Box::new(PngFile(path.clone())), fallback)),
        Output::None => Ok((Box::new(Nowhere), fallback)),
    }
}

/// The pixel formats the framebuffer is driven in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// 16 bits, red in the top five.
    Rgb565,
    /// 32 bits, blue in the low byte and the top byte unused.
    Xrgb8888,
}

/// A Linux framebuffer device.
pub struct Framebuffer {
    file: File,
    width: u32,
    height: u32,
    /// Bytes per row in the device, which can exceed the pixels.
    stride: usize,
    format: Format,
}

impl Framebuffer {
    /// Opens the device and reads its geometry from sysfs.
    pub fn open(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("{}: not a device name", path.display()))?;
        let sysfs = PathBuf::from("/sys/class/graphics").join(name);
        let read = |attr: &str| -> Result<String> {
            let p = sysfs.join(attr);
            std::fs::read_to_string(&p)
                .map(|s| s.trim().to_string())
                .with_context(|| format!("read {}", p.display()))
        };
        let (width, height) = read("virtual_size")?
            .split_once(',')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .context("parse virtual_size")?;
        let bits: u32 = read("bits_per_pixel")?
            .parse()
            .context("parse bits_per_pixel")?;
        let stride: usize = read("stride")?.parse().context("parse stride")?;
        let format = match bits {
            16 => Format::Rgb565,
            32 => Format::Xrgb8888,
            other => bail!(
                "{}: {other} bits per pixel is not supported",
                path.display()
            ),
        };
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        tracing::info!(device = %path.display(), width, height, bits, stride, "framebuffer");
        Ok(Self {
            file,
            width,
            height,
            stride,
            format,
        })
    }

    pub fn size(&self) -> Size {
        Size {
            width: self.width,
            height: self.height,
        }
    }
}

impl Sink for Framebuffer {
    fn present(&mut self, frame: &Pixmap) -> Result<()> {
        if frame.width() != self.width || frame.height() != self.height {
            bail!(
                "frame is {}x{}, framebuffer is {}x{}",
                frame.width(),
                frame.height(),
                self.width,
                self.height
            );
        }
        let bytes = encode(frame, self.format, self.stride);
        self.file
            .seek(SeekFrom::Start(0))
            .context("seek framebuffer")?;
        self.file.write_all(&bytes).context("write framebuffer")
    }
}

/// Packs an opaque pixmap into the device format, row by row at
/// the device stride.
fn encode(frame: &Pixmap, format: Format, stride: usize) -> Vec<u8> {
    let width = frame.width() as usize;
    let height = frame.height() as usize;
    let mut out = vec![0u8; stride * height];
    for (row, pixels) in frame.data().chunks_exact(width * 4).enumerate() {
        let line = &mut out[row * stride..];
        match format {
            Format::Rgb565 => {
                for (px, dst) in pixels.chunks_exact(4).zip(line.chunks_exact_mut(2)) {
                    let r = (px[0] as u16) >> 3;
                    let g = (px[1] as u16) >> 2;
                    let b = (px[2] as u16) >> 3;
                    dst.copy_from_slice(&((r << 11) | (g << 5) | b).to_le_bytes());
                }
            }
            Format::Xrgb8888 => {
                for (px, dst) in pixels.chunks_exact(4).zip(line.chunks_exact_mut(4)) {
                    dst.copy_from_slice(&[px[2], px[1], px[0], 0]);
                }
            }
        }
    }
    out
}

/// A PNG file rewritten with every frame, for looking at the
/// display without one.
pub struct PngFile(PathBuf);

impl Sink for PngFile {
    fn present(&mut self, frame: &Pixmap) -> Result<()> {
        let bytes = frame.encode_png().context("encode PNG")?;
        let tmp = self.0.with_extension("png.tmp");
        std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.0).with_context(|| format!("rename to {}", self.0.display()))
    }
}

/// A sink that drops every frame.
pub struct Nowhere;

impl Sink for Nowhere {
    fn present(&mut self, _frame: &Pixmap) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_parses_its_three_forms() {
        assert_eq!("none".parse::<Output>().unwrap(), Output::None);
        assert_eq!(
            "/dev/fb0".parse::<Output>().unwrap(),
            Output::Framebuffer(PathBuf::from("/dev/fb0"))
        );
        assert_eq!(
            "shot.png".parse::<Output>().unwrap(),
            Output::Png(PathBuf::from("shot.png"))
        );
        assert!("fb0".parse::<Output>().is_err());
    }

    #[test]
    fn size_parses_and_refuses_zero() {
        let size: Size = "1280x400".parse().unwrap();
        assert_eq!((size.width, size.height), (1280, 400));
        assert!("1280".parse::<Size>().is_err());
        assert!("0x400".parse::<Size>().is_err());
    }

    fn two_by_one() -> Pixmap {
        let mut frame = Pixmap::new(2, 1).unwrap();
        frame
            .data_mut()
            .copy_from_slice(&[255, 0, 0, 255, 0, 0, 255, 255]);
        frame
    }

    #[test]
    fn rgb565_packs_red_and_blue_at_the_device_stride() {
        let bytes = encode(&two_by_one(), Format::Rgb565, 8);
        assert_eq!(bytes.len(), 8);
        assert_eq!(&bytes[..4], &[0x00, 0xF8, 0x1F, 0x00]);
        assert_eq!(&bytes[4..], &[0; 4], "padding past the pixels");
    }

    #[test]
    fn xrgb8888_puts_blue_in_the_low_byte() {
        let bytes = encode(&two_by_one(), Format::Xrgb8888, 8);
        assert_eq!(bytes, [0, 0, 255, 0, 255, 0, 0, 0]);
    }

    #[test]
    fn a_png_sink_writes_a_readable_file() {
        let dir = std::env::temp_dir().join(format!("coinbath-png-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("frame.png");
        let mut sink = PngFile(path.clone());
        sink.present(&two_by_one()).unwrap();
        let decoded = Pixmap::load_png(&path).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 1));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
