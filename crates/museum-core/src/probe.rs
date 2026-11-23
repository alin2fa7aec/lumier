//! Reads what the wall needs to know about an image file without decoding it.
//!
//! Only headers are read. Fully decoding every file would change scan time by
//! an order of magnitude, and nothing here needs the pixels.

use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::date::Date;

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("cannot open: {0}")]
    Open(#[source] std::io::Error),

    #[error("cannot read metadata: {0}")]
    Metadata(#[source] std::io::Error),

    #[error("cannot read dimensions: {0}")]
    Dimensions(#[source] image::ImageError),

    #[error("cannot rewind after reading EXIF: {0}")]
    Rewind(#[source] std::io::Error),

    #[error("cannot read the video: {0}")]
    Video(#[source] crate::video::VideoError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    /// Displayed width, with EXIF orientation already applied.
    pub width: u32,
    /// Displayed height, with EXIF orientation already applied.
    pub height: u32,
    /// Short hash of (modification time, length). Changes when the file is
    /// replaced, which is what lets image URLs be cached immutably.
    pub version: String,
    /// Length in seconds. Only videos have one.
    pub duration: Option<f64>,
    /// When it was taken, in seconds since the epoch, UTC. From EXIF for a
    /// photo and the movie header for a video; absent when neither says.
    pub taken: Option<i64>,
    /// When the file was last modified, in seconds since the epoch.
    pub modified: i64,
}

pub fn probe(path: &Path) -> Result<Probe, ProbeError> {
    let file = File::open(path).map_err(ProbeError::Open)?;
    let metadata = file.metadata().map_err(ProbeError::Metadata)?;

    // One open serves both reads. Opening twice would double the round trips
    // on a network mount, where the scan is latency-bound.
    let mut reader = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok();
    let orientation = exif.as_ref().and_then(orientation_of);
    let taken = exif.as_ref().and_then(taken_of);
    reader
        .seek(SeekFrom::Start(0))
        .map_err(ProbeError::Rewind)?;

    let (width, height) = image::ImageReader::new(reader)
        .with_guessed_format()
        .map_err(|error| ProbeError::Dimensions(image::ImageError::IoError(error)))?
        .into_dimensions()
        .map_err(ProbeError::Dimensions)?;

    // Browsers honour EXIF orientation when rendering, so a quarter-turned
    // photo is displayed with its axes swapped. Reporting the raw dimensions
    // would lay out the wall to the wrong shape.
    let (width, height) = if turns_quarter(orientation) {
        (height, width)
    } else {
        (width, height)
    };

    Ok(Probe {
        width,
        height,
        version: version_of(&metadata),
        duration: None,
        taken,
        modified: modified_of(&metadata),
    })
}

/// EXIF orientation values 5 through 8 involve a 90 degree rotation; 1 to 4 are
/// identity, flips, or a half turn, none of which swap the axes.
fn turns_quarter(orientation: Option<u32>) -> bool {
    matches!(orientation, Some(5..=8))
}

pub(crate) fn read_orientation<R: std::io::BufRead + Seek>(reader: &mut R) -> Option<u32> {
    orientation_of(&exif::Reader::new().read_from_container(reader).ok()?)
}

fn orientation_of(exif: &exif::Exif) -> Option<u32> {
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)
}

/// When the photo was taken, as UTC seconds.
///
/// EXIF stores the time as the camera's local clock reading, with the zone in
/// a separate offset tag that phones write and many older cameras do not.
/// Without one the reading is taken as UTC: photos from one camera still
/// sort among themselves, but against a video (whose header is UTC) they can
/// be off by the zone's offset.
fn taken_of(exif: &exif::Exif) -> Option<i64> {
    let pairs = [
        (exif::Tag::DateTimeOriginal, exif::Tag::OffsetTimeOriginal),
        (exif::Tag::DateTimeDigitized, exif::Tag::OffsetTimeDigitized),
    ];
    pairs.iter().find_map(|&(time, offset)| {
        let mut taken = exif::DateTime::from_ascii(ascii_of(exif, time)?).ok()?;
        if let Some(offset) = ascii_of(exif, offset) {
            // A malformed offset leaves the reading as it is, taken as UTC.
            let _ = taken.parse_offset(offset);
        }
        let date = Date::from_ymd(
            i64::from(taken.year),
            u32::from(taken.month),
            u32::from(taken.day),
        )?;
        let clock =
            i64::from(taken.hour) * 3600 + i64::from(taken.minute) * 60 + i64::from(taken.second);
        let offset = i64::from(taken.offset.unwrap_or(0)) * 60;
        Some(date.unix_seconds() + clock - offset)
    })
}

fn ascii_of(exif: &exif::Exif, tag: exif::Tag) -> Option<&[u8]> {
    match &exif.get_field(tag, exif::In::PRIMARY)?.value {
        exif::Value::Ascii(parts) => parts.first().map(Vec::as_slice),
        _ => None,
    }
}

pub(crate) fn modified_of(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_secs() as i64)
}

pub(crate) fn version_of(metadata: &std::fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());

    let mut input = [0u8; 24];
    input[..16].copy_from_slice(&modified.to_le_bytes());
    input[16..].copy_from_slice(&metadata.len().to_le_bytes());

    let digest = blake3::hash(&input);
    digest.to_hex()[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_turns_are_exactly_orientations_five_to_eight() {
        for value in 1..=4 {
            assert!(!turns_quarter(Some(value)), "{value}");
        }
        for value in 5..=8 {
            assert!(turns_quarter(Some(value)), "{value}");
        }
        assert!(!turns_quarter(None));
        assert!(!turns_quarter(Some(9)));
    }

    /// A real JPEG of the given size with an EXIF orientation spliced in.
    ///
    /// The bytes are encoded rather than hand-assembled because the JPEG
    /// decoder reads past the frame header to report dimensions — a stub with
    /// only SOF0 is rejected as a truncated image.
    fn jpeg_with_orientation(orientation: u16, width: u16, height: u16) -> Vec<u8> {
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut std::io::Cursor::new(&mut encoded))
            .encode_image(&image::RgbImage::new(u32::from(width), u32::from(height)))
            .expect("encoding a blank JPEG");

        let mut tiff = Vec::new();
        tiff.extend(b"II"); // little endian
        tiff.extend(42u16.to_le_bytes()); // TIFF magic
        tiff.extend(8u32.to_le_bytes()); // offset of the first IFD
        tiff.extend(1u16.to_le_bytes()); // one entry
        tiff.extend(0x0112u16.to_le_bytes()); // Orientation
        tiff.extend(3u16.to_le_bytes()); // type SHORT
        tiff.extend(1u32.to_le_bytes()); // one value
        tiff.extend(orientation.to_le_bytes());
        tiff.extend([0, 0]); // pad the value field to four bytes
        tiff.extend(0u32.to_le_bytes()); // no next IFD

        let mut app1 = Vec::from(*b"Exif\0\0");
        app1.extend(&tiff);

        // APP1 goes immediately after SOI, where readers expect it.
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend([0xFF, 0xE1]);
        jpeg.extend(
            u16::try_from(app1.len() + 2)
                .expect("APP1 fits a segment")
                .to_be_bytes(),
        );
        jpeg.extend(&app1);
        jpeg.extend(&encoded[2..]);
        jpeg
    }

    fn probe_bytes(name: &str, bytes: &[u8]) -> Probe {
        let dir = std::env::temp_dir().join(format!("lumier-probe-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("image.jpg");
        std::fs::write(&path, bytes).unwrap();
        let probed = probe(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        probed.unwrap()
    }

    #[test]
    fn a_quarter_turned_photo_reports_swapped_dimensions() {
        // Browsers rotate this when rendering, so the wall must be laid out to
        // the rotated shape, not the stored one.
        let probed = probe_bytes("rotated", &jpeg_with_orientation(6, 100, 50));
        assert_eq!((probed.width, probed.height), (50, 100));
    }

    #[test]
    fn an_upright_photo_reports_stored_dimensions() {
        let probed = probe_bytes("upright", &jpeg_with_orientation(1, 100, 50));
        assert_eq!((probed.width, probed.height), (100, 50));

        // A half turn does not swap the axes either.
        let probed = probe_bytes("halfturn", &jpeg_with_orientation(3, 100, 50));
        assert_eq!((probed.width, probed.height), (100, 50));
    }

    /// EXIF holding the given ASCII fields, through the crate's own writer.
    fn exif_with(fields: &[(exif::Tag, &str)]) -> exif::Exif {
        let fields: Vec<exif::Field> = fields
            .iter()
            .map(|&(tag, text)| exif::Field {
                tag,
                ifd_num: exif::In::PRIMARY,
                value: exif::Value::Ascii(vec![text.as_bytes().to_vec()]),
            })
            .collect();
        let mut writer = exif::experimental::Writer::new();
        for field in &fields {
            writer.push_field(field);
        }
        let mut tiff = std::io::Cursor::new(Vec::new());
        writer.write(&mut tiff, false).unwrap();
        exif::Reader::new().read_raw(tiff.into_inner()).unwrap()
    }

    #[test]
    fn reads_when_a_photo_was_taken_in_its_own_zone() {
        let exif = exif_with(&[
            (exif::Tag::DateTimeOriginal, "2026:11:24 09:30:00"),
            (exif::Tag::OffsetTimeOriginal, "+09:00"),
        ]);
        // 00:30 UTC that day.
        assert_eq!(taken_of(&exif), Some(1_795_478_400 + 30 * 60));
    }

    #[test]
    fn takes_a_reading_without_a_zone_as_utc() {
        let exif = exif_with(&[(exif::Tag::DateTimeOriginal, "2026:11:24 09:30:00")]);
        assert_eq!(taken_of(&exif), Some(1_795_478_400 + 9 * 3600 + 30 * 60));
    }

    #[test]
    fn falls_back_to_the_digitized_time_and_ignores_a_blank_one() {
        let exif = exif_with(&[(exif::Tag::DateTimeDigitized, "2026:11:24 00:00:00")]);
        assert_eq!(taken_of(&exif), Some(1_795_478_400));
        let exif = exif_with(&[(exif::Tag::DateTimeOriginal, "0000:00:00 00:00:00")]);
        assert_eq!(taken_of(&exif), None);
    }

    #[test]
    fn version_tracks_length_and_time() {
        let dir = std::env::temp_dir().join(format!("lumier-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.bin");

        std::fs::write(&path, b"one").unwrap();
        let first = version_of(&std::fs::metadata(&path).unwrap());
        assert_eq!(first.len(), 8);

        // Same metadata, same version — this is what makes the URL cacheable.
        assert_eq!(first, version_of(&std::fs::metadata(&path).unwrap()));

        std::fs::write(&path, b"different length").unwrap();
        assert_ne!(first, version_of(&std::fs::metadata(&path).unwrap()));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
