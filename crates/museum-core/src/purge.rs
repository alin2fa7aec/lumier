//! The nightly batch: put back what was asked for, move marked files into the
//! trash, and empty the trash of days past their keeping.
//!
//! This is the only code that moves or deletes library files. It runs as
//! `lumier purge`, in its own process and its own systemd unit, so the
//! server that faces the network never needs write access to the library.
//!
//! The trash keeps each file at `{day}/{library path}`, the day being the one
//! it was moved. Keeping the library's nesting is what makes a restore a
//! single rename back, by this batch or by hand; dating the folder is what
//! lets expiry go by when a file was trashed rather than by its mtime, which a
//! rename does not touch.

use std::path::Path;

use crate::date::Date;
use crate::library::live_photo_movies;
use crate::path::{self, PathError};
use crate::probe::version_of;
use crate::state::{Mark, StateDir, StateError};

pub struct Options<'a> {
    /// Canonical library root.
    pub root: &'a Path,
    /// Canonical trash root, on the same filesystem as the library so that a
    /// move is a rename.
    pub trash: &'a Path,
    pub state: &'a StateDir,
    /// Days a file stays in the trash before it is deleted.
    pub keep_days: u32,
    /// Refuse to move more than this many files in one run. `None` lifts it.
    pub limit: Option<usize>,
    pub today: Date,
}

/// What one run did, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub restored: usize,
    /// Restores refused because a file had since taken the original's place.
    pub restore_blocked: usize,
    pub moved: usize,
    /// Marks dropped because the file was gone or had changed since marking.
    pub stale: usize,
    /// Moves that failed and stay marked for the next run.
    pub failed: usize,
    /// Day folders deleted from the trash.
    pub expired: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum PurgeError {
    #[error(
        "{count} files are marked, more than the limit of {limit}; nothing was moved. \
         Run `lumier purge --no-limit` if that many really should go"
    )]
    OverLimit { count: usize, limit: usize },
    #[error(transparent)]
    State(#[from] StateError),
}

/// Runs the batch under the state directory's lock, so marks made meanwhile
/// wait for it instead of being lost.
pub fn run(options: &Options) -> Result<Report, PurgeError> {
    let (_, report) = options.state.update(|state| {
        if let Some(limit) = options.limit
            && state.marks.len() > limit
        {
            return Err(PurgeError::OverLimit {
                count: state.marks.len(),
                limit,
            });
        }

        let mut report = Report::default();

        // Restores first, so a file put back and marked again in the same
        // day is handled by the mark.
        for trashed in std::mem::take(&mut state.restores) {
            match restore(options, &trashed) {
                Ok(true) => report.restored += 1,
                Ok(false) => report.restore_blocked += 1,
                Err(error) => {
                    tracing::warn!(%trashed, %error, "cannot restore");
                    report.restore_blocked += 1;
                }
            }
        }

        let mut kept = Vec::new();
        for mark in std::mem::take(&mut state.marks) {
            match trash(options, &mark) {
                Ok(Moved::Yes) => report.moved += 1,
                Ok(Moved::Stale) => report.stale += 1,
                Err(error) => {
                    tracing::warn!(path = %mark.path, %error, "cannot move to the trash");
                    report.failed += 1;
                    kept.push(mark);
                }
            }
        }
        state.marks = kept;

        report.expired = expire(options);
        Ok(report)
    })?;
    Ok(report)
}

enum Moved {
    Yes,
    /// Gone, or changed since it was marked: not the file that was meant.
    Stale,
}

fn trash(options: &Options, mark: &Mark) -> Result<Moved, PathError> {
    let source = match path::resolve(options.root, &mark.path) {
        Ok(source) => source,
        Err(PathError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Moved::Stale);
        }
        Err(error) => return Err(error),
    };
    let metadata = std::fs::metadata(&source).map_err(|source| io(&mark.path, source))?;
    if version_of(&metadata) != mark.v {
        tracing::info!(path = %mark.path, "changed since it was marked; leaving it");
        return Ok(Moved::Stale);
    }

    let day = options.today.to_string();
    move_with_movies(
        options.root,
        &mark.path,
        options.trash,
        &format!("{day}/{}", mark.path),
    )?;
    Ok(Moved::Yes)
}

/// Returns false when something now stands where the file came from.
fn restore(options: &Options, trashed: &str) -> Result<bool, PathError> {
    let Some((_day, original)) = trashed.split_once('/') else {
        return Err(PathError::NotRelative(trashed.to_owned()));
    };
    if path::resolve(options.root, original).is_ok() {
        tracing::warn!(%original, "a file has taken its place; leaving it in the trash");
        return Ok(false);
    }
    move_with_movies(options.trash, trashed, options.root, original)?;
    remove_empty_parents(options.trash, trashed);
    Ok(true)
}

/// Removes the folders a restore left empty, up to the day folder itself,
/// so the trash list does not keep a day with nothing in it.
fn remove_empty_parents(trash: &Path, trashed: &str) {
    let mut dir = trashed;
    while let Some((parent, _)) = dir.rsplit_once('/') {
        // remove_dir refuses a folder that still holds anything, which is
        // the stopping condition.
        let removed = path::resolve(trash, parent).map(std::fs::remove_dir);
        if !matches!(removed, Ok(Ok(()))) {
            break;
        }
        dir = parent;
    }
}

/// Moves one file, and the movie half of a Live Photo with it, from one root
/// to another. Every path on both sides goes through the containment checks.
fn move_with_movies(
    from_root: &Path,
    from: &str,
    to_root: &Path,
    to: &str,
) -> Result<(), PathError> {
    let source = path::resolve(from_root, from)?;
    let (from_dir, name) = split(from);
    let (to_dir, _) = split(to);
    let dir = source.parent().unwrap_or(from_root);
    let movies = live_photo_movies(dir, name);

    let target = path::resolve_new(to_root, to)?;
    std::fs::rename(&source, &target).map_err(|error| io(from, error))?;

    for movie in movies {
        let from = join(from_dir, &movie);
        let to = join(to_dir, &movie);
        let moved = path::resolve(from_root, &from)
            .and_then(|source| Ok((source, path::resolve_new(to_root, &to)?)))
            .and_then(|(source, target)| {
                std::fs::rename(source, target).map_err(|error| io(&from, error))
            });
        if let Err(error) = moved {
            tracing::warn!(%from, %error, "cannot move the Live Photo movie with its still");
        }
    }
    Ok(())
}

/// Deletes the day folders that have been kept long enough. Anything in the
/// trash root that is not a day folder is left alone.
fn expire(options: &Options) -> usize {
    let Ok(read) = std::fs::read_dir(options.trash) else {
        return 0;
    };
    let mut expired = 0;
    for item in read.filter_map(Result::ok) {
        let Some(name) = item.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(day) = name.parse::<Date>() else {
            continue;
        };
        if options.today.days_since(day) < i64::from(options.keep_days) {
            continue;
        }
        let removed = path::resolve(options.trash, &name)
            .and_then(|dir| std::fs::remove_dir_all(dir).map_err(|error| io(&name, error)));
        match removed {
            Ok(()) => expired += 1,
            Err(error) => tracing::warn!(day = %name, %error, "cannot empty the trash"),
        }
    }
    expired
}

/// Days left before a trash day folder is deleted, counting the run that
/// deletes it as the last.
pub fn days_left(day: Date, today: Date, keep_days: u32) -> i64 {
    (i64::from(keep_days) - today.days_since(day)).max(0)
}

fn split(rel: &str) -> (&str, &str) {
    rel.rsplit_once('/').unwrap_or(("", rel))
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

fn io(path: &str, source: std::io::Error) -> PathError {
    PathError::Io {
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::fs;

    struct Fixture {
        _dir: TempDir,
        root: std::path::PathBuf,
        trash: std::path::PathBuf,
        state: StateDir,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new();
        let root = dir.path().join("library");
        let trash = dir.path().join("trash");
        fs::create_dir_all(root.join("trip")).unwrap();
        fs::create_dir_all(&trash).unwrap();
        fs::write(root.join("trip/a.jpg"), b"a").unwrap();
        fs::write(root.join("trip/b.jpg"), b"b").unwrap();
        fs::write(root.join("trip/b.mov"), b"movie").unwrap();
        let state = StateDir::open(&dir.path().join("state")).unwrap();
        Fixture {
            root: root.canonicalize().unwrap(),
            trash: trash.canonicalize().unwrap(),
            state,
            _dir: dir,
        }
    }

    fn options<'a>(f: &'a Fixture, today: &str, limit: Option<usize>) -> Options<'a> {
        Options {
            root: &f.root,
            trash: &f.trash,
            state: &f.state,
            keep_days: 30,
            limit,
            today: today.parse().unwrap(),
        }
    }

    fn mark(f: &Fixture, rel: &str) {
        let v = version_of(&fs::metadata(f.root.join(rel)).unwrap());
        f.state
            .update(|s| {
                s.marks.push(Mark::new(rel.into(), v));
                Ok::<_, StateError>(())
            })
            .unwrap();
    }

    #[test]
    fn moves_a_marked_file_and_its_live_photo_movie_into_a_day_folder() {
        let f = fixture();
        mark(&f, "trip/b.jpg");
        let report = run(&options(&f, "2026-11-24", Some(500))).unwrap();
        assert_eq!(report.moved, 1);
        assert!(f.trash.join("2026-11-24/trip/b.jpg").is_file());
        assert!(f.trash.join("2026-11-24/trip/b.mov").is_file());
        assert!(!f.root.join("trip/b.jpg").exists());
        assert!(!f.root.join("trip/b.mov").exists());
        assert!(f.root.join("trip/a.jpg").exists());
        assert!(f.state.read().unwrap().marks.is_empty());
    }

    #[test]
    fn leaves_a_file_that_changed_since_it_was_marked() {
        let f = fixture();
        mark(&f, "trip/a.jpg");
        fs::write(f.root.join("trip/a.jpg"), b"replaced, and longer").unwrap();
        let report = run(&options(&f, "2026-11-24", Some(500))).unwrap();
        assert_eq!((report.moved, report.stale), (0, 1));
        assert!(f.root.join("trip/a.jpg").exists());
        assert!(f.state.read().unwrap().marks.is_empty());
    }

    #[test]
    fn moves_nothing_past_the_limit() {
        let f = fixture();
        mark(&f, "trip/a.jpg");
        mark(&f, "trip/b.jpg");
        let error = run(&options(&f, "2026-11-24", Some(1))).unwrap_err();
        assert!(matches!(
            error,
            PurgeError::OverLimit { count: 2, limit: 1 }
        ));
        assert!(f.root.join("trip/a.jpg").exists());
        assert_eq!(f.state.read().unwrap().marks.len(), 2);
        // Lifting the limit lets them through.
        assert_eq!(run(&options(&f, "2026-11-24", None)).unwrap().moved, 2);
    }

    #[test]
    fn restores_into_the_original_place_with_the_movie() {
        let f = fixture();
        mark(&f, "trip/b.jpg");
        run(&options(&f, "2026-11-24", Some(500))).unwrap();
        f.state
            .update(|s| {
                s.restores.push("2026-11-24/trip/b.jpg".into());
                Ok::<_, StateError>(())
            })
            .unwrap();
        let report = run(&options(&f, "2026-11-25", Some(500))).unwrap();
        assert_eq!(report.restored, 1);
        assert!(f.root.join("trip/b.jpg").is_file());
        assert!(f.root.join("trip/b.mov").is_file());
        assert!(f.state.read().unwrap().restores.is_empty());
        assert!(!f.trash.join("2026-11-24").exists());
    }

    #[test]
    fn will_not_restore_over_a_newer_file() {
        let f = fixture();
        mark(&f, "trip/a.jpg");
        run(&options(&f, "2026-11-24", Some(500))).unwrap();
        fs::write(f.root.join("trip/a.jpg"), b"new").unwrap();
        f.state
            .update(|s| {
                s.restores.push("2026-11-24/trip/a.jpg".into());
                Ok::<_, StateError>(())
            })
            .unwrap();
        let report = run(&options(&f, "2026-11-25", Some(500))).unwrap();
        assert_eq!((report.restored, report.restore_blocked), (0, 1));
        assert_eq!(fs::read(f.root.join("trip/a.jpg")).unwrap(), b"new");
        assert!(f.trash.join("2026-11-24/trip/a.jpg").is_file());
    }

    #[test]
    fn empties_day_folders_once_kept_long_enough() {
        let f = fixture();
        fs::create_dir_all(f.trash.join("2026-10-25/trip")).unwrap();
        fs::create_dir_all(f.trash.join("2026-10-26/trip")).unwrap();
        fs::create_dir_all(f.trash.join("not-a-day")).unwrap();
        let report = run(&options(&f, "2026-11-24", Some(500))).unwrap();
        assert_eq!(report.expired, 1);
        assert!(!f.trash.join("2026-10-25").exists());
        assert!(f.trash.join("2026-10-26").exists());
        assert!(f.trash.join("not-a-day").exists());
    }

    #[test]
    fn counts_days_left_down_to_zero() {
        let day: Date = "2026-11-24".parse().unwrap();
        assert_eq!(days_left(day, day, 30), 30);
        assert_eq!(days_left(day, "2026-12-23".parse().unwrap(), 30), 1);
        assert_eq!(days_left(day, "2027-01-30".parse().unwrap(), 30), 0);
    }
}
