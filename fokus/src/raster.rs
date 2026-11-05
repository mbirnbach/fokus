//! Bitmap images in a design, and how a Photo layer becomes dots.
//!
//! An image keeps the file it came from (PNG, JPEG, …), so a project stays
//! exact; it is decoded once and shared between undo copies. A Photo layer is
//! laid out as a grid of dots, one per line interval in both directions, then
//! dithered to black and white with Jarvis, Judice and Ninke error diffusion
//! (LightBurn's usual choice for photos), or cut at 50 % grey for logos. Each
//! row's runs of black dots are the spans the laser burns.

use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use image::{GrayImage, imageops};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Images are decoded at most this large (longest side, pixels); at 0.1 mm
/// per dot that is still 40 cm.
const MAX_DECODED_PX: u32 = 4096;
/// Most dots one Photo layer may have (about 50 × 50 cm at 0.1 mm).
pub const MAX_DOTS: usize = 25_000_000;

/// An image file, decoded on first use.
#[derive(Clone)]
pub struct ImageData {
    bytes: Arc<Vec<u8>>,
    /// Luminance composited onto white (transparent = white); None if the
    /// file can't be decoded.
    grey: Arc<OnceLock<Option<Arc<GrayImage>>>>,
    /// The last resize, for dots: (size, pixels).
    resized: Arc<Mutex<Option<((u32, u32), Arc<GrayImage>)>>>,
}

impl ImageData {
    /// An image from file contents; fails if it can't be decoded.
    pub fn new(bytes: Vec<u8>) -> Result<Self, String> {
        let data = Self::unchecked(bytes);
        data.grey().map(|_| data).ok_or_else(|| "unsupported or damaged image".to_string())
    }

    fn unchecked(bytes: Vec<u8>) -> Self {
        ImageData { bytes: Arc::new(bytes), grey: Arc::default(), resized: Arc::default() }
    }

    /// The image in grey, 0 = black, 255 = white.
    pub fn grey(&self) -> Option<Arc<GrayImage>> {
        self.grey
            .get_or_init(|| {
                let img = image::load_from_memory(&self.bytes).ok()?;
                let img = if img.width().max(img.height()) > MAX_DECODED_PX { img.thumbnail(MAX_DECODED_PX, MAX_DECODED_PX) } else { img };
                let mut la = img.to_luma_alpha8();
                for p in la.pixels_mut() {
                    let a = p[1] as u32;
                    p[0] = ((p[0] as u32 * a + 255 * (255 - a)) / 255) as u8;
                }
                Some(Arc::new(GrayImage::from_fn(la.width(), la.height(), |x, y| image::Luma([la.get_pixel(x, y)[0]]))))
            })
            .clone()
    }

    /// Pixel size, if it decodes.
    pub fn size(&self) -> Option<(u32, u32)> {
        self.grey().map(|g| g.dimensions())
    }

    /// Identifies the file (for caches); copies of one image share it.
    pub fn key(&self) -> usize {
        Arc::as_ptr(&self.bytes) as usize
    }

    /// The image scaled to `w` × `h` pixels (one per dot).
    pub fn resized(&self, w: u32, h: u32) -> Option<Arc<GrayImage>> {
        let mut last = self.resized.lock().unwrap();
        if let Some((size, img)) = last.as_ref() {
            if *size == (w, h) {
                return Some(img.clone());
            }
        }
        let grey = self.grey()?;
        let img = Arc::new(imageops::resize(&*grey, w.max(1), h.max(1), imageops::FilterType::Triangle));
        *last = Some(((w, h), img.clone()));
        Some(img)
    }
}

impl PartialEq for ImageData {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.bytes, &other.bytes) || self.bytes == other.bytes
    }
}

impl fmt::Debug for ImageData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ImageData({} bytes)", self.bytes.len())
    }
}

/// Stored in projects as the file's bytes in base64.
impl Serialize for ImageData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64.encode(&*self.bytes))
    }
}

impl<'de> Deserialize<'de> for ImageData {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        BASE64.decode(text).map(Self::unchecked).map_err(serde::de::Error::custom)
    }
}

/// How an image's greys turn into dots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ImageMode {
    /// Error diffusion: greys become denser or sparser dots (photos).
    #[default]
    Dither,
    /// Darker than mid-grey burns, the rest doesn't (logos, line art).
    Threshold,
}

/// An image's settings: how it is turned into dots, and tone changes.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ImageLook {
    pub mode: ImageMode,
    /// -1..1; positive is lighter (burns less).
    pub brightness: f32,
    /// -1..1; positive is harder.
    pub contrast: f32,
    /// Burn the light parts instead (e.g. on dark anodised aluminium).
    pub invert: bool,
}

impl ImageLook {
    /// The grey (0..1, 0 = black) a pixel is engraved as.
    pub fn tone(&self, luma: u8) -> f32 {
        let mut v = luma as f32 / 255.0;
        if self.invert {
            v = 1.0 - v;
        }
        let c = self.contrast.clamp(-0.95, 0.95);
        let k = (1.0 + c) / (1.0 - c);
        ((v - 0.5) * k + 0.5 + self.brightness).clamp(0.0, 1.0)
    }

    /// How much a pixel burns: 0 = not at all, 1 = fully.
    pub fn ink(&self, luma: u8) -> f32 {
        1.0 - self.tone(luma)
    }

    /// The pixel as shown on the canvas: the tone, or black and white.
    pub fn preview(&self, luma: u8) -> u8 {
        match self.mode {
            ImageMode::Dither => (self.tone(luma) * 255.0).round() as u8,
            ImageMode::Threshold if self.ink(luma) >= 0.5 => 0,
            ImageMode::Threshold => 255,
        }
    }
}

/// Jarvis, Judice and Ninke weights (/48) to the right on this row and on
/// the next two: (dx, dy, weight).
const JARVIS: [(i32, usize, f32); 12] = [
    (1, 0, 7.0), (2, 0, 5.0),
    (-2, 1, 3.0), (-1, 1, 5.0), (0, 1, 7.0), (1, 1, 5.0), (2, 1, 3.0),
    (-2, 2, 1.0), (-1, 2, 3.0), (0, 2, 5.0), (1, 2, 3.0), (2, 2, 1.0),
];

/// Black and white dots from `ink` (0..1 per dot, row by row, `cols` wide).
/// Dots marked `diffuse` are dithered and pass their error on to other such
/// dots; the rest are cut at 0.5. Rows are processed in alternating
/// directions so the error doesn't drift to one side.
pub fn dither(mut ink: Vec<f32>, diffuse: &[bool], cols: usize) -> Vec<bool> {
    let rows = if cols == 0 { 0 } else { ink.len() / cols };
    let mut on = vec![false; ink.len()];
    for r in 0..rows {
        let rightwards = r % 2 == 0;
        for i in 0..cols {
            let c = if rightwards { i } else { cols - 1 - i };
            let at = r * cols + c;
            let v = ink[at];
            on[at] = v >= 0.5;
            if !diffuse[at] {
                continue;
            }
            let err = v - if on[at] { 1.0 } else { 0.0 };
            for &(dx, dy, w) in &JARVIS {
                let x = c as i32 + if rightwards { dx } else { -dx };
                let y = r + dy;
                if x < 0 || x >= cols as i32 || y >= rows {
                    continue;
                }
                let to = y * cols + x as usize;
                if diffuse[to] {
                    ink[to] += err * w / 48.0;
                }
            }
        }
    }
    on
}

/// A ramp from white on the left to black on the right, for test fields.
pub fn grey_ramp() -> ImageData {
    let img = GrayImage::from_fn(64, 1, |x, _| image::Luma([255 - (x * 255 / 63) as u8]));
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png).expect("writing to memory works");
    ImageData::new(png.into_inner()).expect("the ramp is a valid image")
}

/// Runs of dots that are on in one row, as (first, last + 1) column pairs.
pub fn runs(row: &[bool]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, &v) in row.iter().chain(std::iter::once(&false)).enumerate() {
        match (start, v) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                out.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// A PNG file drawn by `f`, for tests.
#[cfg(test)]
pub fn png(w: u32, h: u32, f: impl Fn(u32, u32) -> image::Rgba<u8>) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, f);
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_pixels_are_white() {
        let data = ImageData::new(png(2, 1, |x, _| if x == 0 { image::Rgba([0, 0, 0, 255]) } else { image::Rgba([0, 0, 0, 0]) })).unwrap();
        let g = data.grey().unwrap();
        assert_eq!((g.get_pixel(0, 0)[0], g.get_pixel(1, 0)[0]), (0, 255));
    }

    #[test]
    fn damaged_files_are_refused() {
        assert!(ImageData::new(b"not an image".to_vec()).is_err());
    }

    #[test]
    fn projects_store_the_file_itself() {
        let bytes = png(3, 2, |_, _| image::Rgba([10, 20, 30, 255]));
        let data = ImageData::new(bytes.clone()).unwrap();
        let json = serde_json::to_string(&data).unwrap();
        let back: ImageData = serde_json::from_str(&json).unwrap();
        assert_eq!(back, data);
        assert_eq!(*back.bytes, bytes);
        assert_eq!(back.size(), Some((3, 2)));
    }

    #[test]
    fn look_changes_the_tone() {
        let plain = ImageLook::default();
        assert_eq!(plain.ink(0), 1.0);
        assert_eq!(plain.ink(255), 0.0);
        assert_eq!(ImageLook { invert: true, ..plain }.ink(0), 0.0);
        assert!(ImageLook { brightness: 0.2, ..plain }.ink(128) < plain.ink(128));
        // More contrast pushes greys apart.
        let hard = ImageLook { contrast: 0.5, ..plain };
        assert!(hard.ink(64) > plain.ink(64) && hard.ink(192) < plain.ink(192));
    }

    #[test]
    fn dithering_keeps_the_grey_level() {
        // A 25 % grey field burns about a quarter of its dots.
        let (cols, rows) = (100, 100);
        let on = dither(vec![0.25; cols * rows], &vec![true; cols * rows], cols);
        let share = on.iter().filter(|&&v| v).count() as f64 / (cols * rows) as f64;
        assert!((share - 0.25).abs() < 0.02, "{share}");
        // Black and white stay as they are.
        assert!(dither(vec![1.0; 50], &[true; 50], 10).iter().all(|&v| v));
        assert!(dither(vec![0.0; 50], &[true; 50], 10).iter().all(|&v| !v));
    }

    #[test]
    fn solid_dots_neither_take_nor_give_error() {
        // A grey dithered dot next to a solid shape's dot (not diffused).
        let on = dither(vec![0.4, 1.0, 0.0], &[true, false, false], 3);
        assert_eq!(on, vec![false, true, false]);
    }

    #[test]
    fn runs_of_dots() {
        assert_eq!(runs(&[true, true, false, true, false, false, true]), vec![(0, 2), (3, 4), (6, 7)]);
        assert_eq!(runs(&[]), vec![]);
    }
}
