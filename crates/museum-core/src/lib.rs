//! Domain layer for lumier: image identity, contained path resolution, the
//! scanned library, and the validated room graph.
//!
//! Deliberately free of any HTTP or UI concern, so the same logic backs the
//! server, `lumier check`, and any future native shell.

pub mod date;
pub mod id;
pub mod library;
pub mod manifest;
pub mod museum;
pub mod path;
pub mod probe;
pub mod purge;
pub mod state;
pub mod thumb;
pub mod video;

#[cfg(test)]
mod testutil;

pub use date::Date;
pub use id::ImageId;
pub use library::{ImageEntry, Kind, Library, ScanError, ScanStats};
pub use manifest::{Manifest, ManifestError, Side};
pub use museum::{Door, Doors, Museum, Room, Validated};
pub use path::PathError;
pub use state::{Mark, State, StateDir, StateError};
pub use thumb::{ThumbError, ThumbSize};
