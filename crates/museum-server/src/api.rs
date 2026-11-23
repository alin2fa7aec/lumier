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
use museum_core::note::{self, LinkIndex};
use museum_core::{Date, ImageEntry, ImageId, Kind, Library, Museum, ThumbSize};

use crate::marks::{self, MarkedPaths, Marking};
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
    /// Entries with no thumbnail: a video's frame is drawn by the client,
    /// and a note hangs as a card.
    unthumbed: HashSet<ImageId>,
    /// Every hung file by name, for resolving the links in notes.
    links: LinkIndex,
    /// The trash, when deletion marks are enabled.
    pub(crate) trash: Option<TrashScan>,
}

/// The trash as scanned: a library of its own, with ids that cannot collide
/// with the library's, grouped by the day each file was moved there.
pub struct TrashScan {
    pub library: Library,
    /// Newest day first.
    pub days: Vec<(Date, Vec<ImageEntry>)>,
}

impl Snapshot {
    pub fn new(
        library: Library,
        museum: Museum,
        images: HashMap<String, Vec<ImageEntry>>,
        trash: Option<TrashScan>,
    ) -> Self {
        let trashed = trash
            .iter()
            .flat_map(|trash| trash.days.iter().flat_map(|(_, images)| images));
        let all = || images.values().flatten().chain(trashed.clone());
        let versions = all().map(|image| (image.id, image.v.clone())).collect();
        let unthumbed = all()
            .filter(|image| matches!(image.kind, Kind::Video | Kind::Markdown))
            .map(|image| image.id)
            .collect();
        let links = LinkIndex::new(
            images
                .iter()
                .map(|(room, entries)| (room.as_str(), entries.as_slice())),
        );
        Self {
            library,
            museum,
            images,
            versions,
            unthumbed,
            links,
            trash,
        }
    }

    pub(crate) fn museum(&self) -> &Museum {
        &self.museum
    }

    pub(crate) fn library(&self) -> &Library {
        &self.library
    }

    pub(crate) fn images_in(&self, room_id: &str) -> &[ImageEntry] {
        self.images.get(room_id).map_or(&[], Vec::as_slice)
    }

    /// The first still in a room, to stand for it where a thumbnail is drawn.
    /// One marked for deletion is passed over: it is on its way out.
    fn first_still(&self, room_id: &str, marked: &MarkedPaths) -> Option<&ImageEntry> {
        self.images_in(room_id)
            .iter()
            .find(|image| image.kind == Kind::Image && !marked.contains(image))
    }

    /// The file behind an id, in the library or else in the trash.
    fn source_path(&self, id: &ImageId) -> Option<std::path::PathBuf> {
        self.library.image_path(id).or_else(|| {
            self.trash
                .as_ref()
                .and_then(|trash| trash.library.image_path(id))
        })
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
    /// Deletion marks and restore requests. `None` keeps the server
    /// read-only, as it was before marks existed.
    pub(crate) marking: Option<Marking>,
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
    pub fn new(
        snapshot: Snapshot,
        loader: Loader,
        thumbs: Thumbnailer,
        marking: Option<Marking>,
    ) -> Self {
        Self {
            snapshot: RwLock::new(Arc::new(snapshot)),
            loader,
            rescanning: tokio::sync::Mutex::new(()),
            thumbs,
            marking,
        }
    }

    /// Paths marked for deletion right now, or none when marking is off.
    pub(crate) fn marked(&self) -> MarkedPaths {
        self.marking
            .as_ref()
            .map_or_else(MarkedPaths::default, Marking::marked)
    }

    /// The current museum. Hold on to the returned `Arc` for the whole of a
    /// request rather than calling this twice, or the two halves could come
    /// from different scans.
    pub(crate) fn current(&self) -> Arc<Snapshot> {
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
        // A purge edits the state files and then asks for this rescan, so
        // the marks it consumed must be read back too.
        if let Some(marking) = &self.marking {
            marking.reload().await?;
        }
        Ok(report)
    }
}

pub fn routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/museum", get(museum))
        .route("/api/rooms/{room_id}", get(room))
        .route("/api/rescan", post(rescan))
        .route("/api/notes/{image_id}", get(note_html))
        .route("/img/{image_id}/{size}", get(image))
        .merge(marks::routes())
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
pub(crate) struct DoorsView<'a> {
    left: Vec<DoorView<'a>>,
    right: Vec<DoorView<'a>>,
}

fn door_view<'a>(state: &'a Snapshot, door: &'a Door, marked: &MarkedPaths) -> DoorView<'a> {
    DoorView {
        to: &door.to,
        name: &door.name,
        peek: state.first_still(&door.to, marked),
    }
}

fn doors_view<'a>(state: &'a Snapshot, doors: &'a Doors, marked: &MarkedPaths) -> DoorsView<'a> {
    DoorsView {
        left: doors
            .left
            .iter()
            .map(|d| door_view(state, d, marked))
            .collect(),
        right: doors
            .right
            .iter()
            .map(|d| door_view(state, d, marked))
            .collect(),
    }
}

/// An entry as the client sees it, with what this server knows about it
/// beyond the file itself.
#[derive(serde::Serialize)]
pub(crate) struct EntryView<'a> {
    #[serde(flatten)]
    pub entry: &'a ImageEntry,
    /// Marked for deletion.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub marked: bool,
    /// In the trash and due to be put back by the next purge.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub restore: bool,
    /// The room it hangs in, on the list of marks that gathers every room.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<&'a str>,
    /// The day it went into the trash, and the days left before it is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_left: Option<i64>,
}

impl<'a> EntryView<'a> {
    pub fn plain(entry: &'a ImageEntry) -> Self {
        Self {
            entry,
            marked: false,
            restore: false,
            room: None,
            day: None,
            days_left: None,
        }
    }
}

#[derive(serde::Serialize)]
struct MuseumView<'a> {
    entrance: &'a str,
    rooms: Vec<MuseumRoomView<'a>>,
    /// Whether this server takes deletion marks, so the client knows to
    /// offer them.
    marking: bool,
}

#[derive(serde::Serialize)]
struct MuseumRoomView<'a> {
    id: &'a str,
    name: &'a str,
    image_count: usize,
    /// Whether the slideshow plays this room.
    slideshow: bool,
    /// One picture to stand for the room on the floor plan.
    cover: Option<&'a ImageEntry>,
    doors: DoorsView<'a>,
}

/// The floor plan: every room and how they join, without any image lists.
async fn museum(State(state): State<Arc<AppState>>) -> Response {
    let marked = state.marked();
    let marking = state.marking.is_some();
    let state = state.current();
    let view = MuseumView {
        marking,
        entrance: state.museum.entrance(),
        rooms: state
            .museum
            .rooms()
            .iter()
            .map(|room| MuseumRoomView {
                id: &room.id,
                name: &room.name,
                image_count: state.images_in(&room.id).len(),
                slideshow: room.slideshow,
                cover: state.first_still(&room.id, &marked),
                doors: doors_view(&state, &room.doors, &marked),
            })
            .collect(),
    };
    Json(view).into_response()
}

#[derive(serde::Serialize)]
pub(crate) struct RoomView<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub images: Vec<EntryView<'a>>,
    pub doors: DoorsView<'a>,
}

impl RoomView<'_> {
    /// A wall that is not a room of the museum and has no doors: the list of
    /// marks, or the trash. The client names it.
    pub fn special<'a>(id: &'a str, images: Vec<EntryView<'a>>) -> RoomView<'a> {
        RoomView {
            id,
            name: "",
            images,
            doors: DoorsView {
                left: Vec::new(),
                right: Vec::new(),
            },
        }
    }
}

async fn room(State(state): State<Arc<AppState>>, Path(room_id): Path<String>) -> Response {
    let marked = state.marked();
    let state = state.current();
    let Some(room) = state.museum.room(&room_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Borrowing out of the snapshot is fine here: `Json` serializes eagerly, so the
    // borrow ends before the owned response is returned.
    Json(RoomView {
        id: &room.id,
        name: &room.name,
        images: state
            .images_in(&room.id)
            .iter()
            .map(|entry| EntryView {
                marked: marked.contains(entry),
                ..EntryView::plain(entry)
            })
            .collect(),
        doors: doors_view(&state, &room.doors, &marked),
    })
    .into_response()
}

#[derive(serde::Serialize)]
struct NoteView {
    /// The note as sanitized HTML, its links pointing at files by id.
    html: String,
}

/// A note rendered for reading, with its links resolved from the room it
/// hangs in.
async fn note_html(State(state): State<Arc<AppState>>, Path(image_id): Path<String>) -> Response {
    let Ok(image_id) = image_id.parse::<ImageId>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let snapshot = state.current();
    let Some(room) = snapshot.images.iter().find_map(|(room, entries)| {
        entries
            .iter()
            .any(|entry| entry.id == image_id && entry.kind == Kind::Markdown)
            .then(|| room.clone())
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(path) = snapshot.library.image_path(&image_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let rendered = tokio::task::spawn_blocking(move || {
        let markdown = note::read(&path)?;
        let rendered = note::render(&markdown, &|name| snapshot.links.resolve(name, &room));
        Ok::<_, std::io::Error>(rendered.html)
    })
    .await;
    match rendered {
        Ok(Ok(html)) => Json(NoteView { html }).into_response(),
        Ok(Err(error)) => {
            tracing::error!(%image_id, %error, "cannot read note");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(error) => {
            tracing::error!(%image_id, %error, "note rendering failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
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
    let Some(source) = snapshot.source_path(&image_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let path = match rendition {
        Rendition::Original => source,
        Rendition::Thumb(size) => {
            if snapshot.unthumbed.contains(&image_id) {
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
