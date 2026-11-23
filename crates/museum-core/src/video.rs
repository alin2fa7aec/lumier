//! Reads what the wall needs to know about a video from its MP4/MOV boxes.
//!
//! `docs/design.md` §6.6 planned to call `ffprobe` for this. Walking the ISO
//! base media boxes directly gives the same three facts — size, rotation and
//! length — without a child process per file or a runtime dependency, and
//! like the image probe it only reads headers: `mdat`, which holds the media
//! itself, is seeked past rather than read.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::probe::{Probe, ProbeError, version_of};

#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("cannot read boxes: {0}")]
    Io(#[from] std::io::Error),

    #[error("malformed box structure")]
    Malformed,

    #[error("no `moov` box")]
    NoMovie,

    #[error("no video track")]
    NoVideoTrack,
}

pub fn probe(path: &Path) -> Result<Probe, ProbeError> {
    let file = File::open(path).map_err(ProbeError::Open)?;
    let metadata = file.metadata().map_err(ProbeError::Metadata)?;
    let mut reader = BufReader::new(file);
    let movie = read_movie(&mut reader, metadata.len()).map_err(ProbeError::Video)?;
    Ok(Probe {
        width: movie.width,
        height: movie.height,
        version: version_of(&metadata),
        duration: movie.duration,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Movie {
    /// Displayed width, rotation already applied.
    pub width: u32,
    /// Displayed height, rotation already applied.
    pub height: u32,
    /// Length in seconds, when the movie header states one.
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct BoxHeader {
    kind: [u8; 4],
    /// Where the payload starts.
    body: u64,
    /// Where the box ends.
    end: u64,
}

fn read_header<R: Read + Seek>(
    reader: &mut R,
    at: u64,
    limit: u64,
) -> Result<BoxHeader, VideoError> {
    reader.seek(SeekFrom::Start(at))?;
    let mut head = [0u8; 8];
    reader.read_exact(&mut head)?;
    let size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
    let kind = [head[4], head[5], head[6], head[7]];
    let (body, end) = match size {
        // Runs to the end of the enclosing box, as `mdat` often does last.
        0 => (at + 8, limit),
        // The real size follows as 64 bits.
        1 => {
            let mut large = [0u8; 8];
            reader.read_exact(&mut large)?;
            (
                at + 16,
                at.checked_add(u64::from_be_bytes(large))
                    .ok_or(VideoError::Malformed)?,
            )
        }
        _ => (at + 8, at + size),
    };
    if end < body || end > limit {
        return Err(VideoError::Malformed);
    }
    Ok(BoxHeader { kind, body, end })
}

/// The boxes directly inside `start..end`.
fn children<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    end: u64,
) -> Result<Vec<BoxHeader>, VideoError> {
    let mut found = Vec::new();
    let mut at = start;
    // Fewer than eight bytes left is padding, not a box.
    while at + 8 <= end {
        let header = read_header(reader, at, end)?;
        found.push(header);
        if header.end <= at {
            return Err(VideoError::Malformed);
        }
        at = header.end;
    }
    Ok(found)
}

fn read_body<R: Read + Seek>(
    reader: &mut R,
    header: BoxHeader,
    max: u64,
) -> Result<Vec<u8>, VideoError> {
    let len = (header.end - header.body).min(max);
    let mut body = vec![0u8; usize::try_from(len).map_err(|_| VideoError::Malformed)?];
    reader.seek(SeekFrom::Start(header.body))?;
    reader.read_exact(&mut body)?;
    Ok(body)
}

fn find<'a>(boxes: &'a [BoxHeader], kind: &[u8; 4]) -> Option<&'a BoxHeader> {
    boxes.iter().find(|header| &header.kind == kind)
}

pub(crate) fn read_movie<R: Read + Seek>(reader: &mut R, len: u64) -> Result<Movie, VideoError> {
    let top = children(reader, 0, len)?;
    let moov = *find(&top, b"moov").ok_or(VideoError::NoMovie)?;
    let in_moov = children(reader, moov.body, moov.end)?;

    let duration = match find(&in_moov, b"mvhd") {
        Some(&mvhd) => movie_duration(&read_body(reader, mvhd, 32)?),
        None => None,
    };

    for trak in in_moov.iter().filter(|header| &header.kind == b"trak") {
        if !is_video_track(reader, *trak)? {
            continue;
        }
        let in_trak = children(reader, trak.body, trak.end)?;
        let Some(&tkhd) = find(&in_trak, b"tkhd") else {
            continue;
        };
        if let Some((width, height)) = track_size(&read_body(reader, tkhd, 96)?) {
            return Ok(Movie {
                width,
                height,
                duration,
            });
        }
    }
    Err(VideoError::NoVideoTrack)
}

/// Whether `trak/mdia/hdlr` names a video handler.
fn is_video_track<R: Read + Seek>(reader: &mut R, trak: BoxHeader) -> Result<bool, VideoError> {
    let in_trak = children(reader, trak.body, trak.end)?;
    let Some(&mdia) = find(&in_trak, b"mdia") else {
        return Ok(false);
    };
    let in_mdia = children(reader, mdia.body, mdia.end)?;
    let Some(&hdlr) = find(&in_mdia, b"hdlr") else {
        return Ok(false);
    };
    // Version and flags, then `pre_defined`, then the handler type.
    let body = read_body(reader, hdlr, 12)?;
    Ok(body.get(8..12) == Some(b"vide"))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// `mvhd`: version 1 widens the times and the duration to 64 bits.
fn movie_duration(mvhd: &[u8]) -> Option<f64> {
    let (timescale, duration) = match mvhd.first()? {
        0 => (be32(mvhd, 12)?, u64::from(be32(mvhd, 16)?)),
        1 => (be32(mvhd, 20)?, be64(mvhd, 24)?),
        _ => return None,
    };
    // All ones means the length is unknown.
    if timescale == 0 || duration == u64::from(u32::MAX) || duration == u64::MAX {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a movie length needs no more than f64 gives"
    )]
    Some(duration as f64 / f64::from(timescale))
}

/// `tkhd`: the presentation size, turned by the transformation matrix.
///
/// A phone filming upright records a landscape frame and a matrix that rotates
/// it a quarter turn, and browsers apply that matrix when playing. Reporting
/// the stored size would lay the wall out to the wrong shape — the same
/// mistake as ignoring EXIF orientation on a photo.
fn track_size(tkhd: &[u8]) -> Option<(u32, u32)> {
    let matrix = match tkhd.first()? {
        0 => 40,
        1 => 52,
        _ => return None,
    };
    let a = be32(tkhd, matrix)?;
    let d = be32(tkhd, matrix + 16)?;
    // 16.16 fixed point; the fraction is dropped.
    let width = be32(tkhd, matrix + 36)? >> 16;
    let height = be32(tkhd, matrix + 40)? >> 16;
    if width == 0 || height == 0 {
        return None;
    }
    // With both diagonal terms zero, the matrix maps x onto y: a quarter turn.
    Some(if a == 0 && d == 0 {
        (height, width)
    } else {
        (width, height)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(body.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        out.extend(kind);
        out.extend(body);
        out
    }

    fn mvhd(timescale: u32, duration: u32) -> Vec<u8> {
        let mut body = vec![0u8; 100];
        body[12..16].copy_from_slice(&timescale.to_be_bytes());
        body[16..20].copy_from_slice(&duration.to_be_bytes());
        boxed(b"mvhd", &body)
    }

    fn tkhd(width: u32, height: u32, quarter_turn: bool) -> Vec<u8> {
        let mut body = vec![0u8; 84];
        let one = 0x0001_0000u32.to_be_bytes();
        let minus_one = (-0x0001_0000i32).to_be_bytes();
        if quarter_turn {
            body[44..48].copy_from_slice(&one); // b
            body[52..56].copy_from_slice(&minus_one); // c
        } else {
            body[40..44].copy_from_slice(&one); // a
            body[56..60].copy_from_slice(&one); // d
        }
        body[76..80].copy_from_slice(&(width << 16).to_be_bytes());
        body[80..84].copy_from_slice(&(height << 16).to_be_bytes());
        boxed(b"tkhd", &body)
    }

    fn trak(handler: &[u8; 4], tkhd: Vec<u8>) -> Vec<u8> {
        let mut hdlr = vec![0u8; 24];
        hdlr[8..12].copy_from_slice(handler);
        let mdia = boxed(b"mdia", &boxed(b"hdlr", &hdlr));
        boxed(b"trak", &[tkhd, mdia].concat())
    }

    fn movie(file: &[u8]) -> Result<Movie, VideoError> {
        read_movie(&mut Cursor::new(file), file.len() as u64)
    }

    #[test]
    fn reads_size_and_length_past_the_media() {
        let moov = boxed(
            b"moov",
            &[
                mvhd(1000, 90_500),
                trak(b"soun", tkhd(0, 0, false)),
                trak(b"vide", tkhd(1920, 1080, false)),
            ]
            .concat(),
        );
        // `moov` after `mdat`, as a camera writes it.
        let file = [boxed(b"ftyp", b"isom"), boxed(b"mdat", &[0u8; 64]), moov].concat();
        assert_eq!(
            movie(&file).unwrap(),
            Movie {
                width: 1920,
                height: 1080,
                duration: Some(90.5)
            }
        );
    }

    #[test]
    fn a_quarter_turned_video_reports_swapped_dimensions() {
        let file = boxed(
            b"moov",
            &[mvhd(600, 600), trak(b"vide", tkhd(1920, 1080, true))].concat(),
        );
        let movie = movie(&file).unwrap();
        assert_eq!((movie.width, movie.height), (1080, 1920));
    }

    #[test]
    fn refuses_a_file_without_a_video_track() {
        let file = boxed(
            b"moov",
            &[mvhd(600, 600), trak(b"soun", tkhd(0, 0, false))].concat(),
        );
        assert!(matches!(movie(&file), Err(VideoError::NoVideoTrack)));
        assert!(matches!(
            movie(&boxed(b"ftyp", b"isom")),
            Err(VideoError::NoMovie)
        ));
    }

    #[test]
    fn refuses_a_box_that_overruns_its_parent() {
        let mut file = boxed(b"moov", &mvhd(600, 600));
        file[3] = 0xFF;
        assert!(matches!(movie(&file), Err(VideoError::Malformed)));
    }
}
