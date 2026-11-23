use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, PoisonError, RwLock};

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use museum_core::museum::{Door, Doors};
use museum_core::{ImageEntry, ImageId, Kind, Library, Museum, ThumbSize};

use crate::thumbs::Thumbnailer;
use tower::ServiceExt;
use tower_http::services::ServeFile;

/// Everything one scan produced. Replaced whole on a rescan and never
/// mutated, so a request that took a snapshot sees one consistent museum
/// even while a rescan swaps in the next.
pub struct Snapshot {
    library: Library,
    museum: Museum,
    /// Room id to the images hanging in it. Rooms with no folder of their own
    /// are simply absent.
    images: HashMap<String, Vec<ImageEntry>>,
    /// Current content version per image, so a request's `?v=` can be checked
    /// before the response is marked immutable.
    versions: HashMap<ImageId, String>,
    /// Videos have no thumbnail: the client draws their frame itself.
    videos: HashSet<ImageId>,
}

impl Snapshot {
    pub fn new(library: Library, museum: Museum, images: HashMap<String, Vec<ImageEntry>>) -> Self {
        let versions = images
            .values()
            .flatten()
            .map(|image| (image.id, image.v.clone()))
            .collect();
        let videos = images
            .values()
            .flatten()
            .filter(|image| image.kind == Kind::Video)
            .map(|image| image.id)
            .collect();
        Self {
            library,
            museum,
            images,
            versions,
            videos,
        }
    }

    fn images_in(&self, room_id: &str) -> &[ImageEntry] {
        self.images.get(room_id).map_or(&[], Vec::as_slice)
    }

    /// The first still in a room, to stand for it where a thumbnail is drawn.
    fn first_still(&self, room_id: &str) -> Option<&ImageEntry> {
        self.images_in(room_id)
            .iter()
            .find(|image| image.kind == Kind::Image)
    }
}

/// Builds a fresh snapshot from disk: re-reads the manifest and rescans every
/// room. Supplied by `main` so that a rescan goes through the same `load()`
/// as startup and `check`.
pub type Loader = Box<dyn Fn() -> anyhow::Result<Snapshot> + Send + Sync>;

pub struct AppState {
    /// The museum as of the last successful scan. The lock is held only long
    /// enough to clone or replace the `Arc`, never across a scan or a request.
    snapshot: RwLock<Arc<Snapshot>>,
    loader: Loader,
    /// Held for the length of a rescan, so two never run at once.
    rescanning: tokio::sync::Mutex<()>,
    /// Outlives every snapshot: its single-flight gates and semaphore must
    /// keep working across a rescan.
    pub thumbs: Thumbnailer,
}

/// Why a rescan did not replace the museum.
#[derive(Debug, thiserror::Error)]
pub enum RescanError {
    #[error("a rescan is already running")]
    Busy,
    /// The manifest no longer builds, or the root cannot be opened. The
    /// previous snapshot stays in service.
    #[error(transparent)]
    Load(#[from] anyhow::Error),
}

/// What a successful rescan found, for the log and the HTTP response.
#[derive(Debug, serde::Serialize)]
pub struct RescanReport {
    pub rooms: usize,
    pub images: usize,
    pub scan_ms: u128,
}

impl AppState {
    pub fn new(snapshot: Snapshot, loader: Loader, thumbs: Thumbnailer) -> Self {
        Self {
            snapshot: RwLock::new(Arc::new(snapshot)),
            loader,
            rescanning: tokio::sync::Mutex::new(()),
            thumbs,
        }
    }

    /// The current museum. Hold on to the returned `Arc` for the whole of a
    /// request rather than calling this twice, or the two halves could come
    /// from different scans.
    fn current(&self) -> Arc<Snapshot> {
        // A poisoned lock only means a writer panicked between two plain
        // assignments; the Arc inside is still whole.
        Arc::clone(&self.snapshot.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Re-reads the manifest and rescans the library, then swaps the result in.
    ///
    /// On failure the previous snapshot keeps serving: a manifest saved
    /// half-edited must not empty the museum.
    pub async fn rescan(self: &Arc<Self>) -> Result<RescanReport, RescanError> {
        let Ok(_guard) = self.rescanning.try_lock() else {
            return Err(RescanError::Busy);
        };
        let started = std::time::Instant::now();
        let this = Arc::clone(self);
        let snapshot = tokio::task::spawn_blocking(move || (this.loader)())
            .await
            .map_err(|error| anyhow::anyhow!("rescan task failed: {error}"))??;
        let report = RescanReport {
            rooms: snapshot.museum.rooms().len(),
            images: snapshot.library.image_count(),
            scan_ms: started.elapsed().as_millis(),
        };
        *self
            .snapshot
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(snapshot);
        Ok(report)
    }
}

pub fn routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/museum", get(museum))
        .route("/api/rooms/{room_id}", get(room))
        .route("/api/rescan", post(rescan))
        .route("/img/{image_id}/{size}", get(image))
        .with_state(state)
}

/// Picks up files added or removed since the last scan, and manifest edits.
///
/// Open to anyone who can reach the server, like everything else here: it
/// changes nothing on disk, and the lock keeps repeated calls from piling up.
async fn rescan(State(state): State<Arc<AppState>>) -> Response {
    match state.rescan().await {
        Ok(report) => {
            tracing::info!(?report, "rescanned");
            Json(report).into_response()
        }
        Err(RescanError::Busy) => {
            (StatusCode::CONFLICT, "a rescan is already running\n").into_response()
        }
        Err(RescanError::Load(error)) => {
            // The details name files under the root, which stay on the
            // server like every other library path.
            tracing::error!("rescan failed, keeping the previous museum: {error:#}");
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "rescan failed; the previous museum is still served. See the server log, or run `lumier check`.\n",
            )
                .into_response()
        }
    }
}

/// A door as the client sees it: where it leads, what the plaque says, and one
/// image from the far side to show through the opening.
#[derive(serde::Serialize)]
struct DoorView<'a> {
    to: &'a str,
    name: &'a str,
    /// One picture from the far side, shown through the opening. The whole
    /// entry rather than just an id, so the client can size and cache it the
    /// same way as anything on a wall.
    peek: Option<&'a ImageEntry>,
}

#[derive(serde::Serialize)]
struct DoorsView<'a> {
    left: Vec<DoorView<'a>>,
    right: Vec<DoorView<'a>>,
}

fn door_view<'a>(state: &'a Snapshot, door: &'a Door) -> DoorView<'a> {
    DoorView {
        to: &door.to,
        name: &door.name,
        peek: state.first_still(&door.to),
    }
}

fn doors_view<'a>(state: &'a Snapshot, doors: &'a Doors) -> DoorsView<'a> {
    DoorsView {
        left: doors.left.iter().map(|d| door_view(state, d)).collect(),
        right: doors.right.iter().map(|d| door_view(state, d)).collect(),
    }
}

#[derive(serde::Serialize)]
struct MuseumView<'a> {
    entrance: &'a str,
    rooms: Vec<MuseumRoomView<'a>>,
}

#[derive(serde::Serialize)]
struct MuseumRoomView<'a> {
    id: &'a str,
    name: &'a str,
    image_count: usize,
    /// One picture to stand for the room on the floor plan.
    cover: Option<&'a ImageEntry>,
    doors: DoorsView<'a>,
}

/// The floor plan: every room and how they join, without any image lists.
async fn museum(State(state): State<Arc<AppState>>) -> Response {
    let state = state.current();
    let view = MuseumView {
        entrance: state.museum.entrance(),
        rooms: state
            .museum
            .rooms()
            .iter()
            .map(|room| MuseumRoomView {
                id: &room.id,
                name: &room.name,
                image_count: state.images_in(&room.id).len(),
                cover: state.first_still(&room.id),
                doors: doors_view(&state, &room.doors),
            })
            .collect(),
    };
    Json(view).into_response()
}

#[derive(serde::Serialize)]
struct RoomView<'a> {
    id: &'a str,
    name: &'a str,
    images: &'a [ImageEntry],
    doors: DoorsView<'a>,
}

async fn room(State(state): State<Arc<AppState>>, Path(room_id): Path<String>) -> Response {
    let state = state.current();
    let Some(room) = state.museum.room(&room_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Borrowing out of the snapshot is fine here: `Json` serializes eagerly, so the
    // borrow ends before the owned response is returned.
    Json(RoomView {
        id: &room.id,
        name: &room.name,
        images: state.images_in(&room.id),
        doors: doors_view(&state, &room.doors),
    })
    .into_response()
}

/// Which rendition of an image to serve.
enum Rendition {
    /// The source file, untouched. Wanted by the lightbox, not by the wall.
    Original,
    /// A WebP capped on its long edge, rendered on demand and cached.
    Thumb(ThumbSize),
}

impl FromStr for Rendition {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "orig" {
            return Ok(Self::Original);
        }
        s.parse::<ThumbSize>().map(Self::Thumb)
    }
}

/// A year, which is the maximum `max-age` worth asking for.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

#[derive(serde::Deserialize)]
struct ImageQuery {
    /// Content version from the room API. Present and current means the bytes
    /// at this URL can never change, so the response is marked immutable.
    v: Option<String>,
}

async fn image(
    State(state): State<Arc<AppState>>,
    Path((image_id, size)): Path<(String, String)>,
    Query(query): Query<ImageQuery>,
    request: Request,
) -> Response {
    let Ok(image_id) = image_id.parse::<ImageId>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(rendition) = size.parse::<Rendition>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let snapshot = state.current();
    let Some(source) = snapshot.library.image_path(&image_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let path = match rendition {
        Rendition::Original => source,
        Rendition::Thumb(size) => {
            if snapshot.videos.contains(&image_id) {
                return StatusCode::NOT_FOUND.into_response();
            }
            // The cache entry is keyed on the version of the file that is on
            // disk now, not on whatever `?v=` the client happened to send: a
            // stale query must never label a freshly rendered thumbnail.
            let version = snapshot
                .versions
                .get(&image_id)
                .map_or_else(|| String::from("0"), Clone::clone);
            let cache_path = state.thumbs.cache_path(&image_id, size, &version);
            if let Err(error) = state.thumbs.ensure(source, cache_path.clone(), size).await {
                tracing::error!(path = %cache_path.display(), %error, "cannot make thumbnail");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            cache_path
        }
    };

    // `ServeFile` gives Range requests, ETag, Last-Modified and conditional
    // GET handling for free — the reason this project is on tower-http.
    let mut response = match ServeFile::new(&path).oneshot(request).await {
        Ok(response) => response.map(Body::new).into_response(),
        Err(error) => {
            tracing::error!(path = %path.display(), %error, "failed to serve image");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Only a matching version earns the immutable mark. A stale or absent `v`
    // falls back to ServeFile's ETag and Last-Modified, which revalidate — if
    // the id alone were marked immutable, a replaced file would be served from
    // cache forever.
    let current = snapshot.versions.get(&image_id);
    if query.v.is_some() && query.v.as_deref() == current.map(String::as_str) {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
    }
    response
}
