//! Domain layer for lumier: image identity, contained path resolution, the
//! scanned library, and the validated room graph.
//!
//! Deliberately free of any HTTP or UI concern, so the same logic backs the
//! server, `lumier check`, and any future native shell.

pub mod id;
pub mod library;
pub mod manifest;
pub mod museum;
pub mod path;
pub mod probe;
pub mod thumb;

pub use id::ImageId;
pub use library::{ImageEntry, Library, ScanError, ScanStats};
pub use manifest::{Manifest, ManifestError, Side};
pub use museum::{Door, Doors, Museum, Room, Validated};
pub use path::PathError;
pub use thumb::{ThumbError, ThumbSize};
