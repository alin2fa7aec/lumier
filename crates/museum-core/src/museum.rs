//! Turns a parsed [`Manifest`] into a validated room graph.
//!
//! Validation is split into errors, which refuse to produce a graph at all, and
//! warnings, which describe a graph that works but probably is not what the
//! author meant. The server runs this at startup and refuses to serve on any
//! error, so a broken manifest can never surface as a runtime surprise.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use crate::library;
use crate::manifest::{Manifest, SUPPORTED_VERSION, Side};
use crate::path as libpath;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Door {
    /// Destination room id.
    pub to: String,
    /// Plaque text: the door's `label`, or the destination room's name.
    pub name: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Doors {
    pub left: Vec<Door>,
    pub right: Vec<Door>,
}

impl Doors {
    fn side_mut(&mut self, side: Side) -> &mut Vec<Door> {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    fn all(&self) -> impl Iterator<Item = &Door> {
        self.left.iter().chain(&self.right)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Room {
    pub id: String,
    pub name: String,
    /// Root-relative folder, or `None` for a room that only joins others.
    /// Never serialized — it would leak library paths to the client.
    #[serde(skip)]
    pub rel_path: Option<String>,
    pub doors: Doors,
}

/// The validated graph.
#[derive(Debug)]
pub struct Museum {
    rooms: Vec<Room>,
    by_id: HashMap<String, usize>,
    entrance: String,
}

impl Museum {
    pub fn entrance(&self) -> &str {
        &self.entrance
    }

    pub fn rooms(&self) -> &[Room] {
        &self.rooms
    }

    pub fn room(&self, id: &str) -> Option<&Room> {
        self.by_id.get(id).map(|&index| &self.rooms[index])
    }
}

#[derive(Debug)]
pub struct Validated {
    pub museum: Museum,
    pub warnings: Vec<String>,
}

/// Validates a manifest against the library root and builds the graph.
///
/// On `Err`, every problem found is reported at once rather than just the
/// first, so one `lumier check` run is enough to fix the file.
pub fn build(manifest: &Manifest, root: &Path) -> Result<Validated, Vec<String>> {
    let mut errors = Vec::new();

    if manifest.version != SUPPORTED_VERSION {
        errors.push(format!(
            "version {} is not supported; this build understands version {SUPPORTED_VERSION}",
            manifest.version
        ));
    }

    // --- rooms -------------------------------------------------------------
    let mut rooms: Vec<Room> = Vec::with_capacity(manifest.rooms.len());
    let mut by_id: HashMap<String, usize> = HashMap::new();

    for definition in &manifest.rooms {
        if let Some(bad) = invalid_id_reason(&definition.id) {
            errors.push(format!("room id {:?}: {bad}", definition.id));
        }
        if by_id.contains_key(&definition.id) {
            errors.push(format!(
                "room id {:?} is declared more than once",
                definition.id
            ));
            continue;
        }

        if let Some(rel_path) = &definition.path {
            match libpath::resolve(root, rel_path) {
                Ok(resolved) if !resolved.is_dir() => errors.push(format!(
                    "room {:?}: path {rel_path:?} is not a directory",
                    definition.id
                )),
                Err(error) => errors.push(format!("room {:?}: {error}", definition.id)),
                Ok(_) => {}
            }
        }

        by_id.insert(definition.id.clone(), rooms.len());
        rooms.push(Room {
            id: definition.id.clone(),
            name: definition.name.clone(),
            rel_path: definition.path.clone(),
            doors: Doors::default(),
        });
    }

    if !by_id.contains_key(&manifest.entrance) {
        errors.push(format!(
            "entrance {:?} is not a declared room",
            manifest.entrance
        ));
    }

    // --- doors -------------------------------------------------------------
    // Hung only once the rooms are known to exist, so a dangling reference is
    // reported rather than panicking on lookup.
    let mut hung: HashSet<(String, String)> = HashSet::new();
    let mut warnings = Vec::new();

    for door in &manifest.doors {
        let mut dangling = false;
        for (role, id) in [("from", &door.from), ("to", &door.to)] {
            if !by_id.contains_key(id) {
                errors.push(format!("door {role}: {id:?} is not a declared room"));
                dangling = true;
            }
        }
        if dangling {
            continue;
        }
        if door.from == door.to {
            errors.push(format!("door from {:?} leads to itself", door.from));
            continue;
        }
        if !errors.is_empty() {
            // Room list may be incomplete; hanging doors now would be noise.
            continue;
        }

        let to_name = rooms[by_id[&door.to]].name.clone();
        let from_name = rooms[by_id[&door.from]].name.clone();

        if !hung.insert((door.from.clone(), door.to.clone())) {
            warnings.push(format!(
                "more than one door leads from {:?} to {:?}",
                door.from, door.to
            ));
        }
        rooms[by_id[&door.from]]
            .doors
            .side_mut(door.side)
            .push(Door {
                to: door.to.clone(),
                name: door.label.clone().unwrap_or(to_name),
            });

        if door.reciprocal {
            if hung.insert((door.to.clone(), door.from.clone())) {
                rooms[by_id[&door.to]]
                    .doors
                    .side_mut(door.side.opposite())
                    .push(Door {
                        to: door.from.clone(),
                        name: from_name,
                    });
            } else {
                warnings.push(format!(
                    "the way back from {:?} to {:?} is already a door; \
                     set reciprocal: false on one of them",
                    door.to, door.from
                ));
            }
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let museum = Museum {
        rooms,
        by_id,
        entrance: manifest.entrance.clone(),
    };

    warnings.extend(unreachable_rooms(&museum));
    warnings.extend(duplicate_paths(&museum));
    warnings.extend(unclaimed_folders(&museum, root));

    Ok(Validated { museum, warnings })
}

fn invalid_id_reason(id: &str) -> Option<&'static str> {
    if id.is_empty() {
        return Some("must not be empty");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Some("may only contain lowercase letters, digits, `_` and `-`");
    }
    None
}

/// Rooms the visitor can never walk to. A door is one-way unless reciprocal,
/// so this walks directed edges.
fn unreachable_rooms(museum: &Museum) -> Vec<String> {
    let mut seen = HashSet::from([museum.entrance.as_str()]);
    let mut queue = VecDeque::from([museum.entrance.as_str()]);

    while let Some(id) = queue.pop_front() {
        let Some(room) = museum.room(id) else {
            continue;
        };
        for door in room.doors.all() {
            if seen.insert(&door.to) {
                queue.push_back(&door.to);
            }
        }
    }

    museum
        .rooms()
        .iter()
        .filter(|room| !seen.contains(room.id.as_str()))
        .map(|room| {
            format!(
                "room {:?} cannot be reached from the entrance {:?}",
                room.id, museum.entrance
            )
        })
        .collect()
}

fn duplicate_paths(museum: &Museum) -> Vec<String> {
    let mut first_claim: HashMap<&str, &str> = HashMap::new();
    let mut warnings = Vec::new();
    for room in museum.rooms() {
        let Some(rel_path) = room.rel_path.as_deref() else {
            continue;
        };
        match first_claim.get(rel_path) {
            Some(other) => warnings.push(format!(
                "rooms {other:?} and {:?} both hang {rel_path:?}",
                room.id
            )),
            None => {
                first_claim.insert(rel_path, &room.id);
            }
        }
    }
    warnings
}

/// Folders under the root that hold images but belong to no room — the check
/// that catches a collection the author forgot to hang.
fn unclaimed_folders(museum: &Museum, root: &Path) -> Vec<String> {
    let claimed: HashSet<&str> = museum
        .rooms()
        .iter()
        .filter_map(|room| room.rel_path.as_deref())
        .collect();

    let mut warnings = Vec::new();
    // Symlinks are not followed, so this cannot be sent in a cycle.
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_dir() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let Some(rel_path) = relative.to_str() else {
            continue;
        };
        if claimed.contains(rel_path) {
            continue;
        }
        // The root itself is spelled "" by a room that hangs it.
        if rel_path.is_empty() && claimed.contains("") {
            continue;
        }
        if holds_images(entry.path()) {
            warnings.push(format!(
                "{} holds images but no room hangs it",
                if rel_path.is_empty() {
                    "the root"
                } else {
                    rel_path
                }
            ));
        }
    }
    warnings.sort();
    warnings
}

fn holds_images(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry.file_type().is_ok_and(|kind| kind.is_file())
            && entry
                .file_name()
                .to_str()
                .is_some_and(library::is_supported_image)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_in(dirs: &[&str], yaml: &str) -> (tmp::Dir, Result<Validated, Vec<String>>) {
        let dir = tmp::Dir::new();
        for sub in dirs {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
            std::fs::write(dir.path().join(sub).join("a.png"), b"x").unwrap();
        }
        let root = libpath::canonical_root(dir.path()).unwrap();
        let manifest = Manifest::parse(yaml).unwrap();
        let result = build(&manifest, &root);
        (dir, result)
    }

    #[test]
    fn hangs_a_reciprocal_door_at_the_opposite_end() {
        let (_dir, result) = build_in(
            &["a", "b"],
            "version: 1\nentrance: a\n\
             rooms: [{id: a, name: A, path: a}, {id: b, name: B, path: b}]\n\
             doors: [{from: a, to: b, side: right}]\n",
        );
        let validated = result.unwrap();

        let a = validated.museum.room("a").unwrap();
        assert_eq!(a.doors.right.len(), 1);
        assert_eq!(a.doors.right[0].to, "b");
        assert_eq!(a.doors.right[0].name, "B");
        assert!(a.doors.left.is_empty());

        // Leaving by a right-hand door must arrive by a left-hand one, or
        // walking back and forth would not preserve direction.
        let b = validated.museum.room("b").unwrap();
        assert_eq!(b.doors.left.len(), 1);
        assert_eq!(b.doors.left[0].to, "a");
        assert!(b.doors.right.is_empty());
        assert!(validated.warnings.is_empty(), "{:?}", validated.warnings);
    }

    #[test]
    fn a_label_overrides_the_destination_name() {
        let (_dir, result) = build_in(
            &["a", "b"],
            "version: 1\nentrance: a\n\
             rooms: [{id: a, name: A, path: a}, {id: b, name: B, path: b}]\n\
             doors: [{from: a, to: b, side: right, label: こちらへ}]\n",
        );
        let validated = result.unwrap();
        assert_eq!(
            validated.museum.room("a").unwrap().doors.right[0].name,
            "こちらへ"
        );
        // Only the outbound plaque is relabelled.
        assert_eq!(validated.museum.room("b").unwrap().doors.left[0].name, "A");
    }

    #[test]
    fn one_way_doors_are_honoured() {
        let (_dir, result) = build_in(
            &["a", "b"],
            "version: 1\nentrance: a\n\
             rooms: [{id: a, name: A, path: a}, {id: b, name: B, path: b}]\n\
             doors: [{from: a, to: b, side: right, reciprocal: false}]\n",
        );
        let validated = result.unwrap();
        assert!(validated.museum.room("b").unwrap().doors.left.is_empty());
    }

    #[test]
    fn reports_every_error_at_once() {
        let (_dir, result) = build_in(
            &["a"],
            "version: 1\nentrance: nowhere\n\
             rooms: [{id: a, name: A, path: a}, {id: a, name: Again}, {id: \"Bad Id\", name: B}]\n\
             doors: [{from: a, to: ghost, side: right}]\n",
        );
        let errors = result.unwrap_err();
        let joined = errors.join("\n");
        assert!(joined.contains("declared more than once"), "{joined}");
        assert!(joined.contains("Bad Id"), "{joined}");
        assert!(joined.contains("entrance"), "{joined}");
        assert!(joined.contains("ghost"), "{joined}");
    }

    #[test]
    fn rejects_a_path_that_is_not_a_directory() {
        let dir = tmp::Dir::new();
        std::fs::write(dir.path().join("photo.png"), b"x").unwrap();
        let root = libpath::canonical_root(dir.path()).unwrap();
        let manifest = Manifest::parse(
            "version: 1\nentrance: a\nrooms: [{id: a, name: A, path: photo.png}]\n",
        )
        .unwrap();
        let errors = build(&manifest, &root).unwrap_err();
        assert!(errors.join("\n").contains("not a directory"));
    }

    #[test]
    fn rejects_a_path_outside_the_root() {
        let (_dir, result) = build_in(
            &["a"],
            "version: 1\nentrance: a\nrooms: [{id: a, name: A, path: ../elsewhere}]\n",
        );
        assert!(result.unwrap_err().join("\n").contains(".."));
    }

    #[test]
    fn warns_about_rooms_cut_off_from_the_entrance() {
        let (_dir, result) = build_in(
            &["a", "b", "c"],
            "version: 1\nentrance: a\n\
             rooms: [{id: a, name: A, path: a}, {id: b, name: B, path: b}, {id: c, name: C, path: c}]\n\
             doors: [{from: b, to: c, side: right}]\n",
        );
        let warnings = result.unwrap().warnings.join("\n");
        assert!(warnings.contains("\"b\" cannot be reached"), "{warnings}");
        assert!(warnings.contains("\"c\" cannot be reached"), "{warnings}");
    }

    #[test]
    fn warns_about_folders_nobody_hangs() {
        let (_dir, result) = build_in(
            &["hung", "forgotten", "forgotten/deeper"],
            "version: 1\nentrance: a\nrooms: [{id: a, name: A, path: hung}]\n",
        );
        let warnings = result.unwrap().warnings.join("\n");
        assert!(warnings.contains("forgotten holds images"), "{warnings}");
        assert!(
            warnings.contains("forgotten/deeper holds images"),
            "{warnings}"
        );
        assert!(!warnings.contains("hung holds images"), "{warnings}");
    }

    #[test]
    fn warns_when_two_rooms_hang_the_same_folder() {
        let (_dir, result) = build_in(
            &["shared"],
            "version: 1\nentrance: a\n\
             rooms: [{id: a, name: A, path: shared}, {id: b, name: B, path: shared}]\n\
             doors: [{from: a, to: b, side: right}]\n",
        );
        let warnings = result.unwrap().warnings.join("\n");
        assert!(warnings.contains("both hang"), "{warnings}");
    }

    #[test]
    fn warns_when_a_reciprocal_door_collides_with_an_explicit_one() {
        let (_dir, result) = build_in(
            &["a", "b"],
            "version: 1\nentrance: a\n\
             rooms: [{id: a, name: A, path: a}, {id: b, name: B, path: b}]\n\
             doors:\n  - {from: a, to: b, side: right}\n  - {from: b, to: a, side: left}\n",
        );
        let warnings = result.unwrap().warnings.join("\n");
        assert!(warnings.contains("already a door"), "{warnings}");
    }

    mod tmp {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU32, Ordering};

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new() -> Self {
                static COUNTER: AtomicU32 = AtomicU32::new(0);
                let path = std::env::temp_dir().join(format!(
                    "lumier-museum-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
