/// Mirrors the server's view types.
export type ImageEntry = {
  id: string
  name: string
  /// Displayed dimensions, EXIF orientation already applied by the server.
  /// Set as the <img> width/height attributes so the browser reserves the
  /// right space before any pixels arrive.
  w: number
  h: number
  /// Content version. Puts the bytes at a URL that can be cached immutably.
  v: string
  /// A video has no thumbnail renditions; only `orig` exists for it. A PDF
  /// hangs as a picture of its first page and opens in the browser's viewer.
  kind: 'image' | 'video' | 'pdf' | 'markdown'
  /// Length in seconds, for videos.
  duration?: number
  /// A note's first heading and opening text, for its card on the wall.
  title?: string
  excerpt?: string
  /// Marked for deletion: the nightly purge will move it to the trash.
  marked?: boolean
  /// In the trash and due to be put back by the next purge.
  restore?: boolean
  /// On the list of marks, the room the picture hangs in.
  room?: string
  /// In the trash, the day it was moved there and the days left before it
  /// is deleted.
  day?: string
  days_left?: number
}

export type Door = {
  to: string
  name: string
  /// One picture from the far side, shown through the opening.
  peek: ImageEntry | null
}

export type Doors = {
  left: Door[]
  right: Door[]
}

export type Room = {
  id: string
  name: string
  images: ImageEntry[]
  doors: Doors
}

export type MuseumRoom = {
  id: string
  name: string
  image_count: number
  /// Whether the slideshow plays this room. The manifest's `slideshow: false`
  /// leaves it out of the tour and takes away the play button there.
  slideshow: boolean
  /// One picture to stand for the room on the floor plan.
  cover: ImageEntry | null
  doors: Doors
}

export type Museum = {
  entrance: string
  rooms: MuseumRoom[]
  /// Whether the server takes deletion marks. Without it the viewer is
  /// read-only and offers none of the marking controls.
  marking: boolean
}

/// Walls that are not rooms of the museum. Room ids are lowercase letters,
/// digits, `_` and `-`, so a `~` can never collide with one.
export const MARKS_WALL = '~marks'
export const TRASH_WALL = '~trash'

export function isSpecialWall(roomId: string): boolean {
  return roomId === MARKS_WALL || roomId === TRASH_WALL
}

async function getJson<T>(url: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(url, { signal })
  if (!response.ok) {
    throw new Error(`Could not load ${url} (HTTP ${response.status})`)
  }
  return response.json() as Promise<T>
}

export function fetchMuseum(signal?: AbortSignal): Promise<Museum> {
  return getJson<Museum>('/api/museum', signal)
}

export async function fetchRoom(roomId: string, signal?: AbortSignal): Promise<Room> {
  if (roomId === MARKS_WALL) {
    return { ...(await getJson<Room>('/api/marks', signal)), name: 'Marked for deletion' }
  }
  if (roomId === TRASH_WALL) {
    return { ...(await getJson<Room>('/api/trash', signal)), name: 'Trash' }
  }
  return getJson<Room>(`/api/rooms/${encodeURIComponent(roomId)}`, signal)
}

/// A note rendered on the server: sanitized HTML whose links point at files
/// by id, as `/img/{id}/orig`.
export function fetchNote(image: ImageEntry, signal?: AbortSignal): Promise<{ html: string }> {
  return getJson<{ html: string }>(`/api/notes/${image.id}?v=${encodeURIComponent(image.v)}`, signal)
}

/// The id a rendered note's link points at, if it points at a file.
export function linkedId(href: string): string | null {
  const match = /^\/img\/([0-9a-f]{32})\//.exec(href)
  return match === null ? null : match[1]
}

/// A note's title on its card: its first heading, else its file name.
export function noteTitle(image: ImageEntry): string {
  return image.title ?? image.name.replace(/\.(md|markdown)$/i, '')
}

async function send(method: 'PUT' | 'DELETE', url: string): Promise<void> {
  const response = await fetch(url, { method })
  if (!response.ok) {
    throw new Error(`Could not save the change (HTTP ${response.status})`)
  }
}

/// Marks a picture for deletion, or takes the mark back.
export function setMark(image: ImageEntry, on: boolean): Promise<void> {
  return send(on ? 'PUT' : 'DELETE', `/api/marks/${image.id}`)
}

/// Asks for a picture in the trash to be put back, or withdraws the request.
export function setRestore(image: ImageEntry, on: boolean): Promise<void> {
  return send(on ? 'PUT' : 'DELETE', `/api/trash/${image.id}/restore`)
}

/// Where a picture on a special wall comes from, for its caption.
export function provenance(image: ImageEntry): string | null {
  if (image.room !== undefined) return image.room
  if (image.day !== undefined) {
    return image.days_left === undefined ? image.day : `${image.day} · ${image.days_left} ${image.days_left === 1 ? 'day' : 'days'} left`
  }
  return null
}

/// Renditions the server will produce, measured on the long edge.
export const THUMB_SIZES = [320, 640, 1280, 2560] as const
export type ThumbSize = (typeof THUMB_SIZES)[number]
/// `orig` is the untouched source file — for the lightbox, never for the wall.
export type Rendition = ThumbSize | 'orig'

/// A video's length as the wall labels it: `4:39`, or `1:02:03` past an hour.
export function formatDuration(seconds: number): string {
  const whole = Math.round(seconds)
  const s = String(whole % 60).padStart(2, '0')
  const m = Math.floor(whole / 60) % 60
  const h = Math.floor(whole / 3600)
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`
}

/// The original carries its file name as a last segment, which the server
/// ignores: a browser titles a PDF tab after the end of its URL, and would
/// otherwise show `orig`.
export function imageUrl(image: ImageEntry, rendition: Rendition): string {
  const path =
    rendition === 'orig' ? `orig/${encodeURIComponent(image.name)}` : String(rendition)
  return `/img/${image.id}/${path}?v=${encodeURIComponent(image.v)}`
}

/// The smallest rendition that still covers how big the picture will be drawn.
///
/// The layout is driven by height — a picture is as tall as its tier and as
/// wide as its aspect ratio makes it — which is why this is computed here
/// instead of handed to the browser as `srcset` + `sizes`: `sizes` can only
/// describe a width.
export function pickRendition(
  image: ImageEntry,
  tierHeight: number,
  devicePixelRatio: number,
): ThumbSize {
  const drawnWidth = tierHeight * (image.w / image.h)
  const needed = Math.max(drawnWidth, tierHeight) * devicePixelRatio
  return THUMB_SIZES.find((size) => size >= needed) ?? THUMB_SIZES[THUMB_SIZES.length - 1]
}
