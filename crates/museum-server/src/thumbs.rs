//! The concurrency control around thumbnail rendering: a disk cache, one
//! render per key no matter how many requests arrive, and a hard bound on how
//! many renders run at once.
//!
//! Each of these guards against a separate failure, and all three are needed:
//!
//! - **The cache** is what makes a wall cheap to revisit.
//! - **Single flight** stops fifty simultaneous requests for one uncached
//!   thumbnail from decoding the same picture fifty times.
//! - **The semaphore** is the one most easily left out. Decoding is blocking
//!   CPU work, so it has to go to `spawn_blocking`; but without a bound, a few
//!   hundred concurrent requests fill the blocking pool and the whole server
//!   stops answering — static files and JSON included, not just images.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use museum_core::{ImageId, ThumbSize, thumb};
use tokio::sync::{Mutex, Semaphore};

#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    #[error("rendering failed: {0}")]
    Render(#[from] thumb::ThumbError),

    #[error("cannot write the cache entry: {0}")]
    Cache(#[source] std::io::Error),

    #[error("the render task did not finish: {0}")]
    Task(#[from] tokio::task::JoinError),
}

pub struct Thumbnailer {
    cache_dir: PathBuf,
    permits: Semaphore,
    /// One gate per cache entry, held for as long as that entry is being
    /// rendered. Entries are removed once nobody is waiting on them.
    inflight: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    /// Makes each temporary file name unique, so two writers cannot land on
    /// the same partial file.
    sequence: AtomicU64,
}

impl Thumbnailer {
    pub fn new(cache_dir: PathBuf) -> Self {
        // Decoding is CPU-bound, so the useful ceiling is the core count.
        let parallelism = std::thread::available_parallelism().map_or(4, |count| count.get());
        tracing::info!(
            cache_dir = %cache_dir.display(),
            render_parallelism = parallelism,
            "thumbnails ready"
        );
        Self {
            cache_dir,
            permits: Semaphore::new(parallelism),
            inflight: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(0),
        }
    }

    /// Where a rendition lives on disk.
    ///
    /// The id's first two characters shard the directory, so a large library
    /// does not put tens of thousands of files in one place. The version is
    /// part of the file name rather than only the URL: a replaced source must
    /// not be served from the old cache entry.
    pub fn cache_path(&self, id: &ImageId, size: ThumbSize, version: &str) -> PathBuf {
        let id = id.to_string();
        self.cache_dir
            .join(size.label())
            .join(&id[..2])
            .join(format!("{id}-{version}.webp"))
    }

    /// Makes sure the cache entry exists, rendering it if needed.
    pub async fn ensure(
        &self,
        source: PathBuf,
        cache_path: PathBuf,
        size: ThumbSize,
    ) -> Result<(), ThumbnailError> {
        if exists(&cache_path).await {
            return Ok(());
        }

        let result = {
            let gate = {
                let mut inflight = self.inflight.lock().await;
                Arc::clone(inflight.entry(cache_path.clone()).or_default())
            };
            let _turn = gate.lock().await;

            // Re-checked under the gate: whoever held it before us may have
            // just rendered this very entry.
            if exists(&cache_path).await {
                Ok(())
            } else {
                self.render(source, &cache_path, size).await
            }
        };

        // Our own handle is dropped by now, so a count of one means the map is
        // the only holder and no other request is waiting on this key.
        let mut inflight = self.inflight.lock().await;
        if inflight
            .get(&cache_path)
            .is_some_and(|gate| Arc::strong_count(gate) == 1)
        {
            inflight.remove(&cache_path);
        }
        result
    }

    async fn render(
        &self,
        source: PathBuf,
        cache_path: &Path,
        size: ThumbSize,
    ) -> Result<(), ThumbnailError> {
        // Held across the blocking render, which is the point: this is what
        // bounds concurrent decodes. `Semaphore::acquire` only fails on close.
        let _permit = self
            .permits
            .acquire()
            .await
            .expect("the thumbnail semaphore is never closed");

        let started = std::time::Instant::now();
        let bytes = tokio::task::spawn_blocking(move || thumb::render(&source, size)).await??;
        tracing::debug!(
            path = %cache_path.display(),
            bytes = bytes.len(),
            ms = started.elapsed().as_millis(),
            "rendered thumbnail"
        );

        self.store(cache_path, &bytes)
            .await
            .map_err(ThumbnailError::Cache)
    }

    /// Writes beside the target and renames into place, so a reader can never
    /// open a half-written thumbnail.
    async fn store(&self, cache_path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(parent) = cache_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let temporary = cache_path.with_extension(format!(
            "{}.{}.part",
            std::process::id(),
            self.sequence.fetch_add(1, Ordering::Relaxed)
        ));
        tokio::fs::write(&temporary, bytes).await?;
        match tokio::fs::rename(&temporary, cache_path).await {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = tokio::fs::remove_file(&temporary).await;
                Err(error)
            }
        }
    }
}

async fn exists(path: &Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

/// Default cache location, following `XDG_CACHE_HOME` when it is set.
pub fn default_cache_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(xdg).join("lumier");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home).join(".cache").join("lumier");
    }
    std::env::temp_dir().join("lumier-cache")
}
