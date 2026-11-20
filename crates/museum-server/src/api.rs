use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get};
use museum_core::museum::{Door, Doors};
use museum_core::{ImageEntry, ImageId, Library, Museum, ThumbSize};

use crate::thumbs::Thumbnailer;
use tower::ServiceExt;
use tower_http::services::ServeFile;

pub struct AppState {
    pub library: Library,
    pub museum: Museum,
    /// Room id to the images hanging in it. Rooms with no folder of their own
    /// are simply absent.
    pub images: HashMap<String, Vec<ImageEntry>>,
    /// Current content version per image, so a request's `?v=` can be checked
    /// before the response is marked immutable.
    versions: HashMap<ImageId, String>,
    pub thumbs: Thumbnailer,
}

impl AppState {
    pub fn new(
        library: Library,
        museum: Museum,
        images: HashMap<String, Vec<ImageEntry>>,
        thumbs: Thumbnailer,
    ) -> Self {
        let versions = images
            .values()
            .flatten()
            .map(|image| (image.id, image.v.clone()))
            .collect();
        Self {
            library,
            museum,
            images,
            versions,
            thumbs,
        }
    }

    fn images_in(&self, room_id: &str) -> &[ImageEntry] {
        self.images.get(room_id).map_or(&[], Vec::as_slice)
    }
}

pub fn routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/museum", get(museum))
        .route("/api/rooms/{room_id}", get(room))
        .route("/img/{image_id}/{size}", get(image))
        .with_state(state)
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

fn door_view<'a>(state: &'a AppState, door: &'a Door) -> DoorView<'a> {
    DoorView {
        to: &door.to,
        name: &door.name,
        peek: state.images_in(&door.to).first(),
    }
}

fn doors_view<'a>(state: &'a AppState, doors: &'a Doors) -> DoorsView<'a> {
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
                cover: state.images_in(&room.id).first(),
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
    let Some(room) = state.museum.room(&room_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Borrowing out of the Arc is fine here: `Json` serializes eagerly, so the
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
    let Some(source) = state.library.image_path(&image_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let path = match rendition {
        Rendition::Original => source,
        Rendition::Thumb(size) => {
            // The cache entry is keyed on the version of the file that is on
            // disk now, not on whatever `?v=` the client happened to send: a
            // stale query must never label a freshly rendered thumbnail.
            let version = state
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
    let current = state.versions.get(&image_id);
    if query.v.is_some() && query.v.as_deref() == current.map(String::as_str) {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
    }
    response
}
