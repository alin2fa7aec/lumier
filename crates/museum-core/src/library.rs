use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::id::ImageId;
use crate::path::{self, PathError};
use crate::probe;

/// Image formats the viewer handles. HEIC and RAW need a C library
/// (`libheif` and friends) and are deliberately out of scope for now.
const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

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
}

impl Library {
    /// Opens a library root, canonicalizing it once so that every later
    /// containment check has a stable prefix to compare against.
    pub fn open(root: &Path) -> Result<Self, PathError> {
        Ok(Self {
            root: path::canonical_root(root)?,
            index: HashMap::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn image_count(&self) -> usize {
        self.index.len()
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
    pub fn scan_room(&mut self, rel_dir: &str) -> Result<(Vec<ImageEntry>, ScanStats), ScanError> {
        let dir = path::resolve(&self.root, rel_dir)?;
        let read = std::fs::read_dir(&dir).map_err(|source| ScanError::ReadDir {
            dir: dir.display().to_string(),
            source,
        })?;

        let mut entries = Vec::new();
        let mut stats = ScanStats::default();

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

            if !item.file_type().is_ok_and(|kind| kind.is_file()) || !is_supported_image(name) {
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

            let probed = match probe::probe(&file) {
                Ok(probed) => probed,
                Err(error) => {
                    tracing::warn!(%rel_path, %error, "skipping unprobeable image");
                    stats.unreadable += 1;
                    continue;
                }
            };

            let id = ImageId::for_rel_path(&rel_path);
            self.index.insert(id, rel_path.clone());
            entries.push(ImageEntry {
                id,
                name: name.to_owned(),
                w: probed.width,
                h: probed.height,
                v: probed.version,
                rel_path,
            });
        }

        // Natural order, so `img2` precedes `img10`. Plain lexicographic sorting
        // scatters any numbered sequence, which is most of a photo library.
        entries.sort_by(|a, b| natord::compare_ignore_case(&a.name, &b.name));
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

pub(crate) fn is_supported_image(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            let ext = ext.to_ascii_lowercase();
            SUPPORTED_EXTENSIONS.contains(&ext.as_str())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_extensions_case_insensitively() {
        assert!(is_supported_image("a.jpg"));
        assert!(is_supported_image("a.JPEG"));
        assert!(is_supported_image("a.WebP"));
        assert!(!is_supported_image("a.heic"));
        assert!(!is_supported_image("a.txt"));
        assert!(!is_supported_image("jpg"));
        assert!(!is_supported_image("a.jpg.txt"));
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
