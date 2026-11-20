use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("path must be relative to the library root: {0}")]
    NotRelative(String),

    #[error("path must not contain `.` or `..` segments: {0}")]
    NonLiteralSegment(String),

    #[error("path resolves outside the library root: {0}")]
    EscapesRoot(String),

    #[error("cannot resolve {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Resolves a root-relative path to an absolute one, refusing anything that
/// would leave the library root.
///
/// Every piece of code in this project that touches the filesystem goes through
/// this function. Keeping it as the single choke point is what makes the
/// containment guarantee auditable: there is one place to get right, not one
/// per call site.
///
/// `root` must already be canonicalized — [`canonical_root`] does that.
pub fn resolve(root: &Path, rel_path: &str) -> Result<PathBuf, PathError> {
    let candidate = Path::new(rel_path);

    // Reject the obvious cases up front for a clearer error than
    // "escapes root", and so that a nonexistent traversal target still fails
    // loudly instead of surfacing as a plain NotFound.
    for component in candidate.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir | Component::ParentDir => {
                return Err(PathError::NonLiteralSegment(rel_path.to_owned()));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(PathError::NotRelative(rel_path.to_owned()));
            }
        }
    }

    // Canonicalizing is the actual guarantee: it resolves symlinks, so a link
    // inside the root pointing out of it is caught here and not by the loop above.
    let resolved = root
        .join(candidate)
        .canonicalize()
        .map_err(|source| PathError::Io {
            path: rel_path.to_owned(),
            source,
        })?;

    if !resolved.starts_with(root) {
        return Err(PathError::EscapesRoot(rel_path.to_owned()));
    }

    Ok(resolved)
}

/// Canonicalizes the library root once, at startup.
pub fn canonical_root(root: &Path) -> Result<PathBuf, PathError> {
    root.canonicalize().map_err(|source| PathError::Io {
        path: root.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds `root/inside/photo.jpg` plus a sibling `outside/secret.txt`.
    fn fixture() -> (tempdir::TempDir, PathBuf) {
        let dir = tempdir::TempDir::new();
        std::fs::create_dir_all(dir.path().join("root/inside")).unwrap();
        std::fs::create_dir_all(dir.path().join("outside")).unwrap();
        std::fs::write(dir.path().join("root/inside/photo.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("outside/secret.txt"), b"x").unwrap();
        let root = canonical_root(&dir.path().join("root")).unwrap();
        (dir, root)
    }

    #[test]
    fn resolves_a_path_inside_the_root() {
        let (_dir, root) = fixture();
        let resolved = resolve(&root, "inside/photo.jpg").unwrap();
        assert!(resolved.starts_with(&root));
        assert!(resolved.ends_with("inside/photo.jpg"));
    }

    #[test]
    fn rejects_traversal_segments() {
        let (_dir, root) = fixture();
        let err = resolve(&root, "../outside/secret.txt").unwrap_err();
        assert!(matches!(err, PathError::NonLiteralSegment(_)));
        // Traversal that lands back inside the root is still refused: the
        // manifest has no reason to spell a path that way.
        let err = resolve(&root, "inside/../inside/photo.jpg").unwrap_err();
        assert!(matches!(err, PathError::NonLiteralSegment(_)));
    }

    #[test]
    fn rejects_absolute_paths() {
        let (_dir, root) = fixture();
        let err = resolve(&root, "/etc/passwd").unwrap_err();
        assert!(matches!(err, PathError::NotRelative(_)));
    }

    #[test]
    #[cfg(unix)]
    fn rejects_symlinks_pointing_out_of_the_root() {
        let (dir, root) = fixture();
        std::os::unix::fs::symlink(dir.path().join("outside"), root.join("escape")).unwrap();

        // No `..` anywhere in the path, so only canonicalization catches this.
        let err = resolve(&root, "escape/secret.txt").unwrap_err();
        assert!(matches!(err, PathError::EscapesRoot(_)), "got {err:?}");
    }

    /// Minimal scratch directory helper, so the crate takes no dev-dependency
    /// just to make a few files.
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU32, Ordering};

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new() -> Self {
                static COUNTER: AtomicU32 = AtomicU32::new(0);
                let unique = format!(
                    "lumier-test-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                );
                let path = std::env::temp_dir().join(unique);
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
