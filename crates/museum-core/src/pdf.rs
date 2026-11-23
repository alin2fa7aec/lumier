//! What the wall needs from a PDF: the size of its first page, and that page
//! drawn as a picture.
//!
//! `docs/design.md` §6.6 planned `pdfium-render`, which binds Google's C++
//! PDFium and loads it at run time. `hayro` is a PDF rasterizer written in
//! Rust, so like the video probe this adds no library to install beside the
//! binary. Drawing only the first page keeps it to one page's worth of work.
//!
//! A PDF is parsed from its whole contents, unlike an image header: the page
//! tree is found through the cross-reference table at the end of the file.
//! Reading every PDF again on each rescan would be wasteful, so the probe
//! remembers what it found per path and content version, and rereads a file
//! only once it has changed.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

use crate::probe::{Probe, ProbeError, modified_of, version_of};

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("cannot read: {0}")]
    Read(#[source] std::io::Error),

    #[error("not a PDF this viewer can read: {0:?}")]
    Load(hayro::hayro_syntax::LoadPdfError),

    #[error("the PDF has no pages")]
    NoPages,

    #[error("the first page has no area")]
    Empty,

    #[error("the renderer failed on this PDF")]
    Panicked,
}

/// First-page sizes already read, by path and content version.
type Sizes = Mutex<HashMap<(PathBuf, String), (u32, u32)>>;

fn sizes() -> &'static Sizes {
    static SIZES: OnceLock<Sizes> = OnceLock::new();
    SIZES.get_or_init(Mutex::default)
}

pub fn probe(path: &Path) -> Result<Probe, ProbeError> {
    let metadata = std::fs::metadata(path).map_err(ProbeError::Metadata)?;
    let version = version_of(&metadata);
    let key = (path.to_path_buf(), version.clone());

    let cached = sizes()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        .copied();
    let (width, height) = match cached {
        Some(size) => size,
        None => {
            let size = first_page_size(path).map_err(ProbeError::Pdf)?;
            sizes()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, size);
            size
        }
    };

    Ok(Probe {
        width,
        height,
        version,
        duration: None,
        // The document information dictionary may carry a creation date, but
        // it records when the file was written, not when anything in it was
        // taken; the modification time serves as well.
        taken: None,
        modified: modified_of(&metadata),
        card: None,
    })
}

/// The first page's displayed size in points, rotation and crop box applied.
/// A point is 1/72 inch, so an A4 page comes out as 595×842.
fn first_page_size(path: &Path) -> Result<(u32, u32), PdfError> {
    let pdf = load(path)?;
    guarded(|| {
        let pages = pdf.pages();
        let page = pages.first().ok_or(PdfError::NoPages)?;
        let (width, height) = page.render_dimensions();
        let (width, height) = (width.round() as u32, height.round() as u32);
        if width == 0 || height == 0 {
            return Err(PdfError::Empty);
        }
        Ok((width, height))
    })
}

/// Draws the first page on white, its long edge `long_edge` pixels.
pub fn render_first_page(path: &Path, long_edge: u32) -> Result<image::RgbaImage, PdfError> {
    let pdf = load(path)?;
    guarded(|| {
        let pages = pdf.pages();
        let page = pages.first().ok_or(PdfError::NoPages)?;
        let (width, height) = page.render_dimensions();
        if width <= 0.0 || height <= 0.0 {
            return Err(PdfError::Empty);
        }
        #[expect(clippy::cast_precision_loss, reason = "a long edge is a few thousand")]
        let scale = long_edge as f32 / width.max(height);
        let pixmap = hayro::render(
            page,
            &RenderCache::new(),
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
            },
        );
        // Painted over opaque white, so the premultiplied pixels are already
        // the straight ones.
        image::RgbaImage::from_raw(
            u32::from(pixmap.width()),
            u32::from(pixmap.height()),
            pixmap.data_as_u8_slice().to_vec(),
        )
        .ok_or(PdfError::Empty)
    })
}

fn load(path: &Path) -> Result<Pdf, PdfError> {
    let bytes = std::fs::read(path).map_err(PdfError::Read)?;
    guarded(|| Pdf::new(bytes).map_err(PdfError::Load))
}

/// Runs a step of PDF handling, turning a panic in the parser or renderer
/// into an error. PDFs come in every state of brokenness, and one bad file
/// must cost its own thumbnail, not the scan or a blocking thread.
fn guarded<T>(step: impl FnOnce() -> Result<T, PdfError>) -> Result<T, PdfError> {
    catch_unwind(AssertUnwindSafe(step)).unwrap_or(Err(PdfError::Panicked))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    /// A one-page PDF with a filled rectangle, written out by hand with the
    /// cross-reference offsets computed, so the test needs no fixture file.
    pub(crate) fn minimal_pdf(width: u32, height: u32) -> Vec<u8> {
        let content = format!("0 0 1 rg 10 10 {} {} re f", width / 2, height / 2);
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Contents 4 0 R >>"
            ),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            pdf.extend(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn reads_the_first_page_size_in_points() {
        let dir = TempDir::new();
        let path = dir.path().join("a4.pdf");
        std::fs::write(&path, minimal_pdf(595, 842)).unwrap();
        let probed = probe(&path).unwrap();
        assert_eq!((probed.width, probed.height), (595, 842));
    }

    #[test]
    fn draws_the_first_page_to_the_asked_long_edge() {
        let dir = TempDir::new();
        let path = dir.path().join("a4.pdf");
        std::fs::write(&path, minimal_pdf(595, 842)).unwrap();
        let page = render_first_page(&path, 320).unwrap();
        assert_eq!(page.height(), 320);
        assert_eq!(page.width(), 226);
        // The corner is the white page; the rectangle near the origin, which
        // PDF puts at the bottom left, is blue.
        assert_eq!(page.get_pixel(page.width() - 1, 0).0, [255, 255, 255, 255]);
        let blue = page.get_pixel(20, page.height() - 20).0;
        assert!(blue[2] > 200 && blue[0] < 50, "{blue:?}");
    }

    #[test]
    fn refuses_what_is_not_a_pdf() {
        let dir = TempDir::new();
        let path = dir.path().join("fake.pdf");
        // Compressed with gzip, as some distributions ship their PDFs.
        std::fs::write(&path, [0x1f, 0x8b, 8, 0, 0, 0, 0, 0]).unwrap();
        assert!(probe(&path).is_err());
    }
}
