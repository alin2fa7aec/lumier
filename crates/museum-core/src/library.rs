use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::id::ImageId;
use crate::manifest::Order;
use crate::path::{self, PathError};
use crate::{probe, video};

/// Image formats the viewer handles. HEIC and RAW need a C library
/// (`libheif` and friends) and are deliberately out of scope for now.
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

/// Video containers whose boxes [`video::probe`] can read. WebM and the like
/// use another container and are not handled yet.
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov"];

/// What a file on the wall is. The client plays a video where it would show
/// an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Video,
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Path(#[from] PathError),

    #[error("cannot read directory {dir}: {source}")]
    ReadDir {
        dir: String,
        #[source]
        source: std::io::Error,
    },
}

/// One image hanging on a wall.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImageEntry {
    pub id: ImageId,
    /// File name, shown in the lightbox.
    pub name: String,
    /// Displayed width, EXIF orientation applied.
    pub w: u32,
    /// Displayed height, EXIF orientation applied.
    pub h: u32,
    /// Content version, for immutable caching of the image URL.
    pub v: String,
    pub kind: Kind,
    /// Length in seconds, for videos.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// Root-relative, `/`-separated. Not exposed over the API.
    #[serde(skip)]
    pub rel_path: String,
}

/// What a scan did, for the log line.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScanStats {
    pub hung: usize,
    /// Files that looked like images but could not be probed.
    pub unreadable: usize,
}

/// The scanned image library: an id-addressed view of the files under one root.
pub struct Library {
    root: PathBuf,
    index: HashMap<ImageId, String>,
    /// Prefixed to each relative path before hashing it into an id, so the
    /// trash, scanned as a library of its own, never hands out an id that
    /// could also name a file in the library proper.
    namespace: &'static str,
}

impl Library {
    /// Opens a library root, canonicalizing it once so that every later
    /// containment check has a stable prefix to compare against.
    pub fn open(root: &Path) -> Result<Self, PathError> {
        Ok(Self {
            root: path::canonical_root(root)?,
            index: HashMap::new(),
            namespace: "",
        })
    }

    /// Opens the trash as a library, with ids that cannot collide with the
    /// library's own.
    pub fn open_trash(root: &Path) -> Result<Self, PathError> {
        Ok(Self {
            namespace: "trash:",
            ..Self::open(root)?
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn image_count(&self) -> usize {
        self.index.len()
    }

    /// The root-relative path an id was registered under. For the server's
    /// own bookkeeping only; it must never reach a client.
    pub fn rel_path(&self, id: &ImageId) -> Option<&str> {
        self.index.get(id).map(String::as_str)
    }

    /// `rel_dir` and every directory beneath it, root-relative, without
    /// following symlinks. For the trash, whose day folders keep the
    /// library's own nesting.
    pub fn subdirs(&self, rel_dir: &str) -> Result<Vec<String>, ScanError> {
        let dir = path::resolve(&self.root, rel_dir)?;
        let mut dirs = Vec::new();
        for item in walkdir::WalkDir::new(&dir).follow_links(false) {
            let item = match item {
                Ok(item) => item,
                Err(error) => {
                    tracing::warn!(dir = %dir.display(), %error, "skipping unreadable entry");
                    continue;
                }
            };
            if !item.file_type().is_dir() {
                continue;
            }
            let Ok(relative) = item.path().strip_prefix(&self.root) else {
                continue;
            };
            let Some(relative) = relative.to_str() else {
                continue;
            };
            dirs.push(relative.replace(std::path::MAIN_SEPARATOR, "/"));
        }
        Ok(dirs)
    }

    /// Scans a single directory and registers what it finds.
    ///
    /// Not recursive: one folder is one room, and a subfolder is a room of its
    /// own. `rel_dir` may be empty to scan the root itself.
    ///
    /// Each file is probed for its dimensions, because the wall cannot be laid
    /// out before the pictures arrive without them. A file that cannot be
    /// probed is left off the wall rather than hung at a guessed size: it could
    /// not have been displayed either.
    pub fn scan_room(
        &mut self,
        rel_dir: &str,
        order: Order,
    ) -> Result<(Vec<ImageEntry>, ScanStats), ScanError> {
        let dir = path::resolve(&self.root, rel_dir)?;
        let read = std::fs::read_dir(&dir).map_err(|source| ScanError::ReadDir {
            dir: dir.display().to_string(),
            source,
        })?;

        let mut candidates = Vec::new();

        for item in read {
            let item = match item {
                Ok(item) => item,
                Err(error) => {
                    tracing::warn!(dir = %dir.display(), %error, "skipping unreadable entry");
                    continue;
                }
            };

            let file_name = item.file_name();
            let Some(name) = file_name.to_str() else {
                // Paths are hashed and compared as UTF-8 throughout, so a name
                // we cannot represent losslessly is skipped rather than mangled.
                tracing::warn!(
                    dir = %dir.display(),
                    name = %file_name.to_string_lossy(),
                    "skipping file with a non-UTF-8 name"
                );
                continue;
            };

            if !item.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            if let Some(kind) = kind_of(name) {
                candidates.push((name.to_owned(), kind));
            }
        }

        // A Live Photo is a still and a short movie under one name. The movie
        // belongs to the still rather than standing as a work of its own.
        let stills: HashSet<String> = candidates
            .iter()
            .filter(|(_, kind)| *kind == Kind::Image)
            .filter_map(|(name, _)| stem_of(name))
            .collect();

        let mut entries = Vec::new();
        let mut stats = ScanStats::default();

        for (name, kind) in candidates {
            if kind == Kind::Video && stem_of(&name).is_some_and(|stem| stills.contains(&stem)) {
                continue;
            }

            let rel_path = if rel_dir.is_empty() {
                name.to_owned()
            } else {
                format!("{rel_dir}/{name}")
            };

            // Resolved rather than joined onto `dir`, so an entry that is a
            // symlink out of the root is refused here and never indexed. Doing
            // it only at serve time would list an image that then 404s.
            let file = match path::resolve(&self.root, &rel_path) {
                Ok(file) => file,
                Err(error) => {
                    tracing::warn!(%rel_path, %error, "skipping file outside the root");
                    stats.unreadable += 1;
                    continue;
                }
            };

            let probed = match kind {
                Kind::Image => probe::probe(&file),
                Kind::Video => video::probe(&file),
            };
            let probed = match probed {
                Ok(probed) => probed,
                Err(error) => {
                    tracing::warn!(%rel_path, %error, "skipping unprobeable file");
                    stats.unreadable += 1;
                    continue;
                }
            };

            let id = ImageId::for_rel_path(&format!("{}{rel_path}", self.namespace));
            self.index.insert(id, rel_path.clone());
            let when = match order {
                Order::Name => 0,
                Order::Mtime => probed.modified,
                Order::Taken => probed.taken.unwrap_or(probed.modified),
            };
            entries.push((
                when,
                ImageEntry {
                    id,
                    name,
                    w: probed.width,
                    h: probed.height,
                    v: probed.version,
                    kind,
                    duration: probed.duration,
                    rel_path,
                },
            ));
        }

        // Natural order, so `img2` precedes `img10`. Plain lexicographic sorting
        // scatters any numbered sequence, which is most of a photo library. By
        // time, the name still breaks ties: a burst shares one second.
        entries.sort_by(|(a_when, a), (b_when, b)| {
            a_when
                .cmp(b_when)
                .then_with(|| natord::compare_ignore_case(&a.name, &b.name))
        });
        let entries: Vec<ImageEntry> = entries.into_iter().map(|(_, entry)| entry).collect();
        stats.hung = entries.len();
        Ok((entries, stats))
    }

    /// Resolves a registered image id back to a file on disk.
    ///
    /// The path still goes through [`path::resolve`] even though it came from
    /// our own index: nothing in this project reaches the filesystem by any
    /// other route, and the file may have been replaced by a symlink since the
    /// scan.
    pub fn image_path(&self, id: &ImageId) -> Option<PathBuf> {
        let rel_path = self.index.get(id)?;
        match path::resolve(&self.root, rel_path) {
            Ok(path) => Some(path),
            Err(error) => {
                tracing::warn!(%rel_path, %error, "indexed image no longer resolves");
                None
            }
        }
    }
}

/// What a file name says the file is, or `None` for a file the wall skips.
pub(crate) fn kind_of(name: &str) -> Option<Kind> {
    let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        Some(Kind::Image)
    } else if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        Some(Kind::Video)
    } else {
        None
    }
}

/// The movie half of a Live Photo, if `name` is a still that has one in
/// `dir`: the files that are hidden from the wall because they belong to it,
/// and so must move wherever it moves.
pub fn live_photo_movies(dir: &Path, name: &str) -> Vec<String> {
    if kind_of(name) != Some(Kind::Image) {
        return Vec::new();
    }
    let Some(stem) = stem_of(name) else {
        return Vec::new();
    };
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(Result::ok)
        .filter(|item| item.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|item| item.file_name().into_string().ok())
        .filter(|other| {
            kind_of(other) == Some(Kind::Video) && stem_of(other).as_deref() == Some(&stem)
        })
        .collect()
}

/// The name without its extension, folded for comparison.
fn stem_of(name: &str) -> Option<String> {
    Some(Path::new(name).file_stem()?.to_str()?.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_extensions_case_insensitively() {
        assert_eq!(kind_of("a.jpg"), Some(Kind::Image));
        assert_eq!(kind_of("a.JPEG"), Some(Kind::Image));
        assert_eq!(kind_of("a.WebP"), Some(Kind::Image));
        assert_eq!(kind_of("a.MP4"), Some(Kind::Video));
        assert_eq!(kind_of("a.mov"), Some(Kind::Video));
        assert_eq!(kind_of("a.heic"), None);
        assert_eq!(kind_of("a.txt"), None);
        assert_eq!(kind_of("jpg"), None);
        assert_eq!(kind_of("a.jpg.txt"), None);
    }

    #[test]
    fn orders_a_room_by_name_or_by_time() {
        use std::time::{Duration, UNIX_EPOCH};

        let dir = crate::testutil::TempDir::new();
        let room = dir.path().join("room");
        std::fs::create_dir_all(&room).unwrap();
        // Named in one order, modified in another.
        for (name, secs) in [("a.png", 2_000), ("b.png", 3_000), ("c.png", 1_000)] {
            let path = room.join(name);
            image::RgbImage::new(1, 1).save(&path).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
        }
        let mut library = Library::open(dir.path()).unwrap();
        let mut names = |order| -> Vec<String> {
            let (entries, _) = library.scan_room("room", order).unwrap();
            entries.into_iter().map(|entry| entry.name).collect()
        };
        assert_eq!(names(Order::Name), ["a.png", "b.png", "c.png"]);
        assert_eq!(names(Order::Mtime), ["c.png", "a.png", "b.png"]);
        // Nothing here carries EXIF, so taken falls back to the mtime.
        assert_eq!(names(Order::Taken), ["c.png", "a.png", "b.png"]);
    }

    #[test]
    fn orders_names_naturally() {
        let mut names = [
            "img10.png",
            "img2.png",
            "IMG1.png",
            "img_20.png",
            "img_3.png",
        ];
        names.sort_by(|a, b| natord::compare_ignore_case(a, b));
        assert_eq!(
            names,
            [
                "IMG1.png",
                "img2.png",
                "img10.png",
                "img_3.png",
                "img_20.png"
            ]
        );
    }
}
