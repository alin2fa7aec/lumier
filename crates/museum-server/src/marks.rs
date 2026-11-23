//! Deletion marks and restore requests over HTTP.
//!
//! The server only records what the visitor wants: a mark on a file to be
//! moved to the trash, or a request to put a trashed file back. It writes
//! those to the state directory and nothing else. `lumier purge` reads them
//! at night and is the only thing that moves files, so this server never
//! needs write access to the library.
//!
//! Marks are keyed by library path and the content version at marking time.
//! A mark whose version no longer matches the file is shown as unmarked and
//! skipped by the purge: whatever replaced the file under that name is not
//! the file anyone marked.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use museum_core::{Date, ImageEntry, ImageId, Mark, StateDir, purge};

use crate::api::{AppState, EntryView, RoomView};

/// The client's id for the wall that gathers every mark.
const MARKS_WALL: &str = "~marks";
/// The client's id for the wall that shows the trash.
const TRASH_WALL: &str = "~trash";

pub struct Marking {
    state: Arc<StateDir>,
    /// The state as last read or written. Every change re-reads the files
    /// under the lock first, so a purge's edits are never written over.
    current: Mutex<museum_core::State>,
    /// Days a file stays in the trash, for the countdown shown on it.
    keep_days: u32,
}

/// Marked paths with the version each was marked at.
#[derive(Default)]
pub struct MarkedPaths(HashMap<String, String>);

impl MarkedPaths {
    pub fn contains(&self, entry: &ImageEntry) -> bool {
        self.0.get(&entry.rel_path) == Some(&entry.v)
    }
}

impl Marking {
    pub fn open(state: StateDir, keep_days: u32) -> anyhow::Result<Self> {
        let current = state.read()?;
        Ok(Self {
            state: Arc::new(state),
            current: Mutex::new(current),
            keep_days,
        })
    }

    pub fn marked(&self) -> MarkedPaths {
        let current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        MarkedPaths(
            current
                .marks
                .iter()
                .map(|mark| (mark.path.clone(), mark.v.clone()))
                .collect(),
        )
    }

    fn restores(&self) -> Vec<String> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .restores
            .clone()
    }

    /// Re-reads the state files, after a purge has consumed some of them.
    pub async fn reload(&self) -> anyhow::Result<()> {
        let state = Arc::clone(&self.state);
        let read = tokio::task::spawn_blocking(move || state.read()).await??;
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = read;
        Ok(())
    }

    /// Applies one change under the state directory's lock, off the async
    /// threads since the lock may wait for a purge to finish.
    async fn change(
        &self,
        edit: impl FnOnce(&mut museum_core::State) + Send + 'static,
    ) -> anyhow::Result<()> {
        let state = Arc::clone(&self.state);
        let (written, ()) = tokio::task::spawn_blocking(move || {
            state.update(|current| {
                edit(current);
                Ok::<_, museum_core::StateError>(())
            })
        })
        .await??;
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = written;
        Ok(())
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    let writes = Router::new()
        .route("/api/marks/{image_id}", put(add_mark).delete(remove_mark))
        .route(
            "/api/trash/{image_id}/restore",
            put(add_restore).delete(remove_restore),
        )
        .layer(middleware::from_fn(same_origin));
    Router::new()
        .route("/api/marks", get(list_marks))
        .route("/api/trash", get(list_trash))
        .merge(writes)
}

/// Every mark, gathered on one wall in the order the rooms come in the
/// manifest, each labelled with its room.
async fn list_marks(State(state): State<Arc<AppState>>) -> Response {
    if state.marking.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let marked = state.marked();
    let snapshot = state.current();
    let images = snapshot
        .museum()
        .rooms()
        .iter()
        .flat_map(|room| {
            snapshot
                .images_in(&room.id)
                .iter()
                .filter(|entry| marked.contains(entry))
                .map(|entry| EntryView {
                    marked: true,
                    room: Some(&room.name),
                    ..EntryView::plain(entry)
                })
        })
        .collect();
    Json(RoomView::special(MARKS_WALL, images)).into_response()
}

/// The trash, newest day first, each picture carrying its day and the days
/// left before the purge deletes it.
async fn list_trash(State(state): State<Arc<AppState>>) -> Response {
    let Some(marking) = &state.marking else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let restores = marking.restores();
    let snapshot = state.current();
    let today = Date::today();
    let images = snapshot
        .trash
        .iter()
        .flat_map(|trash| &trash.days)
        .flat_map(|(day, entries)| {
            let left = purge::days_left(*day, today, marking.keep_days);
            entries.iter().map(move |entry| (day, left, entry))
        })
        .map(|(day, left, entry)| EntryView {
            restore: restores.contains(&entry.rel_path),
            day: Some(day.to_string()),
            days_left: Some(left),
            ..EntryView::plain(entry)
        })
        .collect();
    Json(RoomView::special(TRASH_WALL, images)).into_response()
}

async fn add_mark(state: State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    set_mark(state, id, true).await
}

async fn remove_mark(state: State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    set_mark(state, id, false).await
}

async fn set_mark(State(state): State<Arc<AppState>>, id: String, on: bool) -> Response {
    let Some(marking) = &state.marking else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(id) = id.parse::<ImageId>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let snapshot = state.current();
    // Only what hangs on a wall can be marked; the path comes from our own
    // index, never from the request.
    let Some(path) = snapshot.library().rel_path(&id).map(str::to_owned) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(v) = snapshot
        .museum()
        .rooms()
        .iter()
        .flat_map(|room| snapshot.images_in(&room.id))
        .find(|entry| entry.id == id)
        .map(|entry| entry.v.clone())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let result = marking
        .change(move |current| {
            current.marks.retain(|mark| mark.path != path);
            if on {
                current.marks.push(Mark::new(path, v));
            }
        })
        .await;
    respond(result)
}

async fn add_restore(state: State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    set_restore(state, id, true).await
}

async fn remove_restore(state: State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    set_restore(state, id, false).await
}

async fn set_restore(State(state): State<Arc<AppState>>, id: String, on: bool) -> Response {
    let Some(marking) = &state.marking else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(id) = id.parse::<ImageId>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let snapshot = state.current();
    let Some(path) = snapshot
        .trash
        .as_ref()
        .and_then(|trash| trash.library.rel_path(&id))
        .map(str::to_owned)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let result = marking
        .change(move |current| {
            current.restores.retain(|restore| *restore != path);
            if on {
                current.restores.push(path);
            }
        })
        .await;
    respond(result)
}

fn respond(result: anyhow::Result<()>) -> Response {
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            // The details name the state directory, which stays on the server.
            tracing::error!("cannot record the change: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Refuses a change sent from a page on another site.
///
/// There is no login: anyone on the LAN may mark, and every mark can be taken
/// back. What this stops is some unrelated page open in the visitor's own
/// browser sending the request on its behalf. The writes are PUT and DELETE,
/// so a cross-origin page is already stopped by the CORS preflight; this
/// check is the second line, for a browser that sends the request anyway.
/// A request without an Origin (curl, say) is not from a page and passes.
async fn same_origin(request: Request, next: Next) -> Response {
    let headers = request.headers();
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin_host = origin
            .to_str()
            .ok()
            .and_then(|origin| origin.split_once("://"))
            .map(|(_, host)| host);
        let host = headers
            .get(header::HOST)
            .and_then(|host| host.to_str().ok());
        if origin_host.is_none() || origin_host != host {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    next.run(request).await
}
