//! The deletion marks and restore requests, kept as two small JSON files in a
//! state directory outside the library.
//!
//! `serve` writes here and nowhere else; `lumier purge` reads these files and
//! is the only thing that moves files. Both take a lock on the directory for
//! every read-modify-write, and the purge holds it for its whole run, so a
//! mark made during a purge waits for it rather than racing it.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MARKS: &str = "marks.json";
const RESTORES: &str = "restores.json";
const LOCK: &str = ".lock";

/// A file someone wants gone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mark {
    /// Library-relative path.
    pub path: String,
    /// The content version when it was marked. The purge moves the file only
    /// if this still matches, so a file replaced under the same name since is
    /// never the one that goes.
    pub v: String,
    /// When it was marked, in seconds since the epoch.
    pub at: u64,
}

impl Mark {
    pub fn new(path: String, v: String) -> Self {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        Self { path, v, at }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub marks: Vec<Mark>,
    /// Trash-relative paths (`{date}/{library path}`) to put back.
    pub restores: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("cannot {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not valid: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

pub struct StateDir {
    dir: PathBuf,
}

impl StateDir {
    /// Opens the state directory, creating it if needed.
    pub fn open(dir: &Path) -> Result<Self, StateError> {
        std::fs::create_dir_all(dir).map_err(io_error("create", dir))?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// The current state, under a shared lock.
    pub fn read(&self) -> Result<State, StateError> {
        let lock = self.lock_file()?;
        lock.lock_shared().map_err(io_error("lock", &self.dir))?;
        self.load()
    }

    /// Reads the state, lets `change` edit it, and writes it back if `change`
    /// succeeded and altered anything, all under one exclusive lock. Returns
    /// the state as it now stands, with `change`'s own result.
    pub fn update<R, E>(
        &self,
        change: impl FnOnce(&mut State) -> Result<R, E>,
    ) -> Result<(State, R), E>
    where
        E: From<StateError>,
    {
        let lock = self.lock_file()?;
        lock.lock().map_err(io_error("lock", &self.dir))?;
        let before = self.load()?;
        let mut state = before.clone();
        let result = change(&mut state)?;
        if state.marks != before.marks {
            self.store(MARKS, &state.marks)?;
        }
        if state.restores != before.restores {
            self.store(RESTORES, &state.restores)?;
        }
        Ok((state, result))
    }

    /// Opened fresh each time: the lock is released when the handle drops,
    /// which keeps every critical section scoped to one call.
    fn lock_file(&self) -> Result<File, StateError> {
        let path = self.dir.join(LOCK);
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io_error("open", &path))
    }

    fn load(&self) -> Result<State, StateError> {
        Ok(State {
            marks: self.load_one(MARKS)?,
            restores: self.load_one(RESTORES)?,
        })
    }

    fn load_one<T: serde::de::DeserializeOwned + Default>(
        &self,
        name: &str,
    ) -> Result<T, StateError> {
        let path = self.dir.join(name);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| StateError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(T::default()),
            Err(error) => Err(io_error("read", &path)(error)),
        }
    }

    /// Written beside the target and renamed into place, so a crash never
    /// leaves half a file to be misread as an empty list.
    fn store<T: serde::Serialize>(&self, name: &str, value: &T) -> Result<(), StateError> {
        let path = self.dir.join(name);
        let part = self.dir.join(format!("{name}.part"));
        let bytes = serde_json::to_vec_pretty(value).expect("state serializes");
        std::fs::write(&part, bytes).map_err(io_error("write", &part))?;
        std::fs::rename(&part, &path).map_err(io_error("replace", &path))
    }
}

fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> StateError {
    let path = path.display().to_string();
    move |source| StateError::Io {
        action,
        path,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn starts_empty_and_keeps_what_was_written() {
        let dir = TempDir::new();
        let state = StateDir::open(&dir.path().join("state")).unwrap();
        assert_eq!(state.read().unwrap(), State::default());

        state
            .update(|s| {
                s.marks.push(Mark::new("a/b.jpg".into(), "v1".into()));
                s.restores.push("2026-11-24/a/c.jpg".into());
                Ok::<_, StateError>(())
            })
            .unwrap();

        let read = state.read().unwrap();
        assert_eq!(read.marks.len(), 1);
        assert_eq!(read.marks[0].path, "a/b.jpg");
        assert_eq!(read.restores, ["2026-11-24/a/c.jpg"]);
    }

    #[test]
    fn a_failed_change_writes_nothing() {
        let dir = TempDir::new();
        let state = StateDir::open(dir.path()).unwrap();
        let result: Result<(State, ()), StateError> = state.update(|s| {
            s.marks.push(Mark::new("a.jpg".into(), "v".into()));
            Err(StateError::Io {
                action: "test",
                path: String::new(),
                source: io::Error::other("refused"),
            })
        });
        assert!(result.is_err());
        assert!(state.read().unwrap().marks.is_empty());
        assert!(!dir.path().join(MARKS).exists());
    }
}
