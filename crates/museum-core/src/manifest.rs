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
        // `order` belongs to a later stage; accepting and ignoring it would be
        // worse than refusing it.
        let error =
            Manifest::parse("version: 1\nentrance: a\nrooms: [{id: a, name: A, order: mtime}]\n")
                .unwrap_err();
        assert!(error.to_string().contains("order"), "{error}");
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
