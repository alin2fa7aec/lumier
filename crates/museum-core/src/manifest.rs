//! The on-disk `museum.yaml`: rooms as nodes, doors as edges.
//!
//! This module only parses. Everything that needs the filesystem or the graph
//! to make sense lives in [`crate::museum`].

use std::path::Path;

/// The only schema version this build understands.
pub const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not valid museum YAML: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_norway::Error,
    },
}

#[derive(Debug, serde::Deserialize)]
// Unknown keys are rejected rather than ignored: a typo, or a field from a
// later stage that this build would silently drop, should fail loudly.
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    /// Room the visitor starts in.
    pub entrance: String,
    #[serde(default)]
    pub rooms: Vec<RoomDef>,
    #[serde(default)]
    pub doors: Vec<DoorDef>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomDef {
    pub id: String,
    pub name: String,
    /// Root-relative folder whose images hang in this room. Omit for a room
    /// that only connects other rooms — a lobby or a corridor.
    #[serde(default)]
    pub path: Option<String>,
    /// How the room's pictures are ordered along the wall.
    #[serde(default)]
    pub order: Order,
}

/// How a room's pictures are ordered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    /// By file name, naturally: `img2` before `img10`.
    #[default]
    Name,
    /// By when the file was last modified, oldest first.
    Mtime,
    /// By when the picture was taken, oldest first: EXIF for a photo, the
    /// movie header for a video, the modification time for anything that
    /// states neither.
    Taken,
}

/// A door, written once as the edge it is.
///
/// Declaring doors per-room instead would let `A` claim a connection to `B`
/// that `B` does not reciprocate; as an edge list that inconsistency cannot be
/// expressed.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoorDef {
    pub from: String,
    pub to: String,
    /// Which end of `from`'s wall the door stands at.
    pub side: Side,
    /// Overrides the destination room's name on the door plaque.
    #[serde(default)]
    pub label: Option<String>,
    /// Also hang a door back to `from` at the opposite end of `to`'s wall.
    #[serde(default = "yes")]
    pub reciprocal: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text).map_err(|source| ManifestError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    pub fn parse(text: &str) -> Result<Self, serde_norway::Error> {
        serde_norway::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
version: 1
entrance: lobby
rooms:
  - id: lobby
    name: エントランス
  - id: hall
    name: 大広間
    path: photos/hall
doors:
  - { from: lobby, to: hall, side: right }
"#;

    #[test]
    fn parses_a_minimal_manifest() {
        let manifest = Manifest::parse(MINIMAL).unwrap();
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.entrance, "lobby");
        assert_eq!(manifest.rooms.len(), 2);
        assert_eq!(manifest.rooms[0].path, None);
        assert_eq!(manifest.rooms[1].path.as_deref(), Some("photos/hall"));
        assert_eq!(manifest.doors[0].side, Side::Right);
    }

    #[test]
    fn doors_are_reciprocal_unless_told_otherwise() {
        let manifest = Manifest::parse(MINIMAL).unwrap();
        assert!(manifest.doors[0].reciprocal);

        let one_way = Manifest::parse(
            "version: 1\nentrance: a\nrooms: [{id: a, name: A}]\n\
             doors: [{from: a, to: a, side: left, reciprocal: false}]\n",
        )
        .unwrap();
        assert!(!one_way.doors[0].reciprocal);
    }

    #[test]
    fn rejects_unknown_keys() {
        // A key parsed but not honoured is worse than one refused.
        let error =
            Manifest::parse("version: 1\nentrance: a\nrooms: [{id: a, name: A, sort: mtime}]\n")
                .unwrap_err();
        assert!(error.to_string().contains("sort"), "{error}");
    }

    #[test]
    fn reads_the_order_and_defaults_to_name() {
        let manifest = Manifest::parse(
            "version: 1\nentrance: a\nrooms: [{id: a, name: A, order: taken}, {id: b, name: B}]\n",
        )
        .unwrap();
        assert_eq!(manifest.rooms[0].order, Order::Taken);
        assert_eq!(manifest.rooms[1].order, Order::Name);
        assert!(
            Manifest::parse("version: 1\nentrance: a\nrooms: [{id: a, name: A, order: exif}]\n")
                .is_err()
        );
    }

    #[test]
    fn rejects_an_unknown_side() {
        assert!(
            Manifest::parse(
                "version: 1\nentrance: a\nrooms: [{id: a, name: A}]\n\
                 doors: [{from: a, to: a, side: up}]\n"
            )
            .is_err()
        );
    }
}
