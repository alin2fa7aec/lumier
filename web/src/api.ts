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
  /// A video has no thumbnail renditions; only `orig` exists for it.
  kind: 'image' | 'video'
  /// Length in seconds, for videos.
  duration?: number
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
  /// One picture to stand for the room on the floor plan.
  cover: ImageEntry | null
  doors: Doors
}

export type Museum = {
  entrance: string
  rooms: MuseumRoom[]
}

async function getJson<T>(url: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(url, { signal })
  if (!response.ok) {
    throw new Error(`${url} を読めませんでした (HTTP ${response.status})`)
  }
  return response.json() as Promise<T>
}

export function fetchMuseum(signal?: AbortSignal): Promise<Museum> {
  return getJson<Museum>('/api/museum', signal)
}

export function fetchRoom(roomId: string, signal?: AbortSignal): Promise<Room> {
  return getJson<Room>(`/api/rooms/${encodeURIComponent(roomId)}`, signal)
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

export function imageUrl(image: ImageEntry, rendition: Rendition): string {
  return `/img/${image.id}/${rendition}?v=${encodeURIComponent(image.v)}`
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
