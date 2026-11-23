//! Rendering one thumbnail: decode, straighten, resize, encode.
//!
//! Synchronous and self-contained on purpose. The concurrency control that a
//! server needs around this — a disk cache, collapsing duplicate requests, and
//! a bound on how many of these run at once — lives in the server, because it
//! is a property of serving many clients rather than of making a thumbnail.

use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::Path;
use std::str::FromStr;

use crate::library::{Kind, kind_of};
use crate::{pdf, probe};

/// WebP quality. High enough that screenshot text and UI edges stay clean,
/// which is where a too-low setting shows first.
const QUALITY: f32 = 82.0;

/// The renditions that can be asked for, measured on the long edge.
///
/// A fixed set rather than an arbitrary `?w=`: open-ended sizes scatter the
/// cache without bound and give the client nothing stable to choose between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThumbSize {
    S320,
    S640,
    S1280,
    S2560,
}

impl ThumbSize {
    pub const ALL: [Self; 4] = [Self::S320, Self::S640, Self::S1280, Self::S2560];

    pub fn long_edge(self) -> u32 {
        match self {
            Self::S320 => 320,
            Self::S640 => 640,
            Self::S1280 => 1280,
            Self::S2560 => 2560,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::S320 => "320",
            Self::S640 => "640",
            Self::S1280 => "1280",
            Self::S2560 => "2560",
        }
    }
}

impl FromStr for ThumbSize {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|size| size.label() == s)
            .ok_or(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ThumbError {
    #[error("cannot open: {0}")]
    Open(#[source] std::io::Error),

    #[error("cannot rewind after reading EXIF: {0}")]
    Rewind(#[source] std::io::Error),

    #[error("cannot decode: {0}")]
    Decode(#[source] image::ImageError),

    #[error("cannot resize: {0}")]
    Resize(#[source] fast_image_resize::ResizeError),

    #[error("cannot encode WebP: {0}")]
    Encode(String),

    #[error("cannot draw the PDF: {0}")]
    Pdf(#[source] crate::pdf::PdfError),
}

/// Decodes `path` and returns a WebP of it, no larger than `size` on its long
/// edge.
///
/// Never upscales: asking for 2560 from a 1440-tall picture re-encodes it at
/// full size, which is still worth doing — a WebP of a screenshot is a small
/// fraction of the source PNG.
pub fn render(path: &Path, size: ThumbSize) -> Result<Vec<u8>, ThumbError> {
    // A PDF page is vector art, so it is drawn straight at the asked size
    // rather than decoded and scaled down.
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
    if kind_of(name) == Some(Kind::Pdf) {
        let page = pdf::render_first_page(path, size.long_edge()).map_err(ThumbError::Pdf)?;
        return encode_webp(&image::DynamicImage::ImageRgb8(
            image::DynamicImage::ImageRgba8(page).to_rgb8(),
        ));
    }

    let file = File::open(path).map_err(ThumbError::Open)?;
    let mut reader = BufReader::new(file);

    // Same single-open approach as probing: one file handle serves the EXIF
    // read and the decode.
    let orientation = probe::read_orientation(&mut reader);
    reader
        .seek(SeekFrom::Start(0))
        .map_err(ThumbError::Rewind)?;

    let decoded = image::ImageReader::new(reader)
        .with_guessed_format()
        .map_err(|error| ThumbError::Decode(image::ImageError::IoError(error)))?
        .decode()
        .map_err(ThumbError::Decode)?;

    // Straightened before resizing. Rotating at full size costs a copy that
    // resizing first would avoid, but doing it in this order keeps the target
    // dimensions obvious, and the copy is small next to the decode.
    let source = straighten(decoded, orientation);

    let (target_width, target_height) = fit(source.width(), source.height(), size.long_edge());
    let resized = if (target_width, target_height) == (source.width(), source.height()) {
        source
    } else {
        let mut destination = image::DynamicImage::new(target_width, target_height, source.color());
        fast_image_resize::Resizer::new()
            .resize(&source, &mut destination, None)
            .map_err(ThumbError::Resize)?;
        destination
    };

    encode_webp(&resized)
}

/// Applies an EXIF orientation so the pixels come out the way they are meant
/// to be seen.
///
/// Values 5 and 7 are diagonal flips, not plain rotations: 5 is a transpose
/// (a rotation plus a horizontal flip) and 7 is a transverse flip.
fn straighten(image: image::DynamicImage, orientation: Option<u32>) -> image::DynamicImage {
    match orientation {
        Some(2) => image.fliph(),
        Some(3) => image.rotate180(),
        Some(4) => image.flipv(),
        Some(5) => image.rotate90().fliph(),
        Some(6) => image.rotate90(),
        Some(7) => image.rotate270().fliph(),
        Some(8) => image.rotate270(),
        // 1, absent, or out of range: already upright.
        _ => image,
    }
}

/// Scales `width`×`height` down so its long edge is at most `long_edge`,
/// leaving it alone if it already fits.
fn fit(width: u32, height: u32, long_edge: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= long_edge || longest == 0 {
        return (width, height);
    }
    let scale = f64::from(long_edge) / f64::from(longest);
    let scaled = |value: u32| ((f64::from(value) * scale).round() as u32).max(1);
    (scaled(width), scaled(height))
}

fn encode_webp(image: &image::DynamicImage) -> Result<Vec<u8>, ThumbError> {
    let (width, height) = (image.width(), image.height());

    // libwebp takes RGB or RGBA only. Alpha is kept when the source has it, so
    // a transparent PNG does not come back with a black background.
    let encoded = if image.color().has_alpha() {
        let pixels = image.to_rgba8();
        webp::Encoder::from_rgba(&pixels, width, height).encode(QUALITY)
    } else {
        let pixels = image.to_rgb8();
        webp::Encoder::from_rgb(&pixels, width, height).encode(QUALITY)
    };

    if encoded.is_empty() {
        return Err(ThumbError::Encode(format!(
            "libwebp produced nothing for {width}x{height}"
        )));
    }
    Ok(encoded.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(name: &str, width: u32, height: u32) -> (std::path::PathBuf, Scratch) {
        let scratch = Scratch::new(name);
        let path = scratch.0.join("source.png");
        let mut pixels = image::RgbImage::new(width, height);
        // A gradient rather than a flat fill, so a resize that silently does
        // nothing would still be visible in the encoded size.
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
        }
        pixels.save(&path).unwrap();
        (path, scratch)
    }

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("lumier-thumb-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dimensions_of_webp(bytes: &[u8]) -> (u32, u32) {
        let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::WebP)
            .expect("output is a readable WebP");
        (decoded.width(), decoded.height())
    }

    #[test]
    fn fits_the_long_edge_without_upscaling() {
        assert_eq!(fit(2560, 1440, 640), (640, 360));
        assert_eq!(fit(1440, 2560, 640), (360, 640));
        assert_eq!(fit(1000, 1000, 320), (320, 320));
        // Already smaller: left alone rather than stretched.
        assert_eq!(fit(200, 100, 640), (200, 100));
        assert_eq!(fit(640, 480, 640), (640, 480));
        // A very wide strip keeps at least one pixel of height.
        assert_eq!(fit(10_000, 3, 320), (320, 1));
    }

    #[test]
    fn renders_a_smaller_webp() {
        let (path, _scratch) = write_png("downscale", 1200, 600);
        let bytes = render(&path, ThumbSize::S320).unwrap();
        assert_eq!(dimensions_of_webp(&bytes), (320, 160));

        // The point of the exercise: far smaller than the source.
        let source_len = std::fs::metadata(&path).unwrap().len() as usize;
        assert!(
            bytes.len() < source_len / 4,
            "{} vs {source_len}",
            bytes.len()
        );
    }

    #[test]
    fn re_encodes_at_full_size_when_the_source_is_smaller() {
        let (path, _scratch) = write_png("noupscale", 400, 300);
        let bytes = render(&path, ThumbSize::S2560).unwrap();
        assert_eq!(dimensions_of_webp(&bytes), (400, 300));
    }

    #[test]
    fn straightening_swaps_the_axes_for_quarter_turns() {
        let upright = image::DynamicImage::new_rgb8(100, 50);
        for orientation in [1, 2, 3, 4] {
            let out = straighten(upright.clone(), Some(orientation));
            assert_eq!((out.width(), out.height()), (100, 50), "{orientation}");
        }
        for orientation in [5, 6, 7, 8] {
            let out = straighten(upright.clone(), Some(orientation));
            assert_eq!((out.width(), out.height()), (50, 100), "{orientation}");
        }
    }

    #[test]
    fn sizes_round_trip_through_their_labels() {
        for size in ThumbSize::ALL {
            assert_eq!(size.label().parse::<ThumbSize>(), Ok(size));
        }
        assert!("480".parse::<ThumbSize>().is_err());
        assert!("orig".parse::<ThumbSize>().is_err());
    }
}
