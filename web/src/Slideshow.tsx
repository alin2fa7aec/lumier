import { Pause, Timer, X } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'

import { fetchRoom, imageUrl, THUMB_SIZES, type ImageEntry, type Museum, type Room } from './api'

/// How long each picture can stay up once it is fully shown, in seconds,
/// offered in a dropdown. Doubling from a minute suits a photo frame, where
/// the useful range runs from a glance to a picture left up for a while.
const INTERVALS = [30, 60, 120, 240, 480, 960] as const
type Interval = (typeof INTERVALS)[number]
const DEFAULT_INTERVAL: Interval = 30
/// Kept in localStorage rather than per tab like the wall positions: the pace
/// suits the screen it is shown on, and a photo frame should come back up at
/// the pace it was left at. Guarded, as storage can be unavailable.
const INTERVAL_KEY = 'lumier:slideshow-interval'

function recallInterval(): Interval {
  try {
    const stored = Number(window.localStorage.getItem(INTERVAL_KEY))
    return INTERVALS.find((seconds) => seconds === stored) ?? DEFAULT_INTERVAL
  } catch {
    return DEFAULT_INTERVAL
  }
}

function rememberInterval(seconds: Interval): void {
  try {
    window.localStorage.setItem(INTERVAL_KEY, String(seconds))
  } catch {
    // A convenience; the show runs at the chosen pace either way.
  }
}

function intervalLabel(seconds: Interval): string {
  return seconds < 60 ? `${seconds}秒` : `${seconds / 60}分`
}
/// How long the controls stay up after the pointer last moved.
const CONTROLS_LINGER = 2500

/// A picture's place in the museum.
export type Position = { roomId: string; index: number }

type Slide = { serial: number; room: Room; index: number; image: ImageEntry; src: string }

/// Every room in the order a visitor would walk them: depth first from the
/// entrance, onward doors before the ones leading back, then any room no door
/// reaches, in manifest order, so the floor plan's jump-only rooms still play.
///
/// The walk follows the manifest's doors rather than its room list because
/// the doors are the curation: a hub's right end lists its branches in the
/// order they were meant to be visited.
function tourOrder(museum: Museum): string[] {
  const rooms = new Map(museum.rooms.map((room) => [room.id, room]))
  const order: string[] = []
  const seen = new Set<string>()
  const visit = (id: string) => {
    const room = rooms.get(id)
    if (room === undefined || seen.has(id)) return
    seen.add(id)
    order.push(id)
    for (const door of [...room.doors.right, ...room.doors.left]) visit(door.to)
  }
  visit(museum.entrance)
  for (const room of museum.rooms) visit(room.id)
  return order
}

/// The first still at or after `index` going in `delta`'s direction, or null
/// past the end of the room. Videos are passed over: the show crossfades
/// pictures on a timer, and a film has neither a rendition to fade nor a
/// length that fits the interval.
function nextStill(room: Room, index: number, delta: 1 | -1): number | null {
  for (let i = index; i >= 0 && i < room.images.length; i += delta) {
    if (room.images[i].kind !== 'video') return i
  }
  return null
}

/// The smallest rendition that fills the screen.
///
/// A rendition rather than `orig`, unlike the lightbox: a slideshow is a
/// stream of requests nobody asked for one by one, and a camera original is
/// several times the bytes for no visible gain at screen size. A picture is
/// never drawn longer than the screen's long edge, whichever way it is turned.
function screenRendition(): (typeof THUMB_SIZES)[number] {
  const needed = Math.max(window.innerWidth, window.innerHeight) * window.devicePixelRatio
  return THUMB_SIZES.find((size) => size >= needed) ?? THUMB_SIZES[THUMB_SIZES.length - 1]
}

/// Plays the whole museum, one picture at a time, crossfading between them.
///
/// Stopping hands back where it got to, so the caller can leave the visitor
/// in front of that picture rather than where the slideshow began.
export function Slideshow({
  museum,
  start,
  onStop,
  onExit,
}: {
  museum: Museum
  start: Position
  /// Back to the lightbox, at the picture on screen.
  onStop: (at: Position) => void
  /// Back to the wall of the room the picture on screen hangs in.
  onExit: (at: Position) => void
}) {
  const order = useMemo(() => tourOrder(museum), [museum])
  const counts = useMemo(
    () => new Map(museum.rooms.map((room) => [room.id, room.image_count])),
    [museum],
  )

  const [slides, setSlides] = useState<{ current: Slide | null; previous: Slide | null }>({
    current: null,
    previous: null,
  })
  const [error, setError] = useState<string | null>(null)
  const [awake, setAwake] = useState(true)
  const [pace, setPace] = useState<Interval>(recallInterval)
  const choosePace = (seconds: number) => {
    const chosen = INTERVALS.find((candidate) => candidate === seconds)
    if (chosen === undefined) return
    setPace(chosen)
    rememberInterval(chosen)
  }

  // Rooms are fetched as the show reaches them and kept, so a second lap
  // costs nothing. A failed fetch is forgotten, so the next lap retries it.
  const rooms = useRef(new Map<string, Promise<Room>>())
  const loadRoom = useCallback((id: string): Promise<Room> => {
    let room = rooms.current.get(id)
    if (room === undefined) {
      room = fetchRoom(id).catch((cause: unknown) => {
        rooms.current.delete(id)
        throw cause
      })
      rooms.current.set(id, room)
    }
    return room
  }, [])

  /// The picture `delta` steps from `from`, crossing into the neighbouring
  /// room in tour order at either end and wrapping round the museum. Empty
  /// rooms are passed over; the floor plan's counts mean most are skipped
  /// without being fetched.
  const step = useCallback(
    async (from: Position, delta: 1 | -1): Promise<{ room: Room; index: number }> => {
      const here = await loadRoom(from.roomId)
      const index = nextStill(here, from.index + delta, delta)
      if (index !== null) return { room: here, index }

      const at = Math.max(order.indexOf(from.roomId), 0)
      // Up to a full lap, which comes back to this room when it is the only
      // one with pictures.
      for (let k = 1; k <= order.length; k++) {
        const id = order[(at + delta * k + order.length * k) % order.length]
        if (counts.get(id) === 0) continue
        const room = await loadRoom(id)
        const first = nextStill(room, delta > 0 ? 0 : room.images.length - 1, delta)
        if (first === null) continue
        return { room, index: first }
      }
      throw new Error('流せる写真がありません')
    },
    [loadRoom, order, counts],
  )

  // Only the latest request may land, so a key pressed while the next picture
  // is still loading is not overtaken by the timer's older one.
  const latest = useRef(0)
  const timer = useRef<number | undefined>(undefined)
  const serial = useRef(0)

  const go = useCallback(
    async (from: Position, delta: 1 | -1) => {
      window.clearTimeout(timer.current)
      const mine = ++latest.current
      try {
        const { room, index } = await step(from, delta)
        const image = room.images[index]
        const src = imageUrl(image, screenRendition())
        // Decoded before it is swapped in, so the fade never runs into a
        // picture that is still arriving. A picture that fails to decode is
        // shown anyway rather than stalling the show on it.
        const loader = new Image()
        loader.src = src
        await loader.decode().catch(() => undefined)
        if (mine !== latest.current) return
        const slide = { serial: ++serial.current, room, index, image, src }
        setSlides((shown) => ({ current: slide, previous: shown.current }))
      } catch (cause) {
        if (mine !== latest.current) return
        setError(cause instanceof Error ? cause.message : String(cause))
      }
    },
    [step],
  )

  // Opens at the starting picture, or the first one after it when the start
  // is an empty room — the entrance is often a lobby with nothing on the wall.
  useEffect(() => {
    // Both refs are counters and handles, not DOM nodes, so reading them at
    // cleanup is exactly what is wanted: whatever is pending by then.
    const requests = latest
    const pending = timer
    void go({ roomId: start.roomId, index: start.index - 1 }, 1)
    return () => {
      requests.current++
      window.clearTimeout(pending.current)
    }
    // Starting again whenever `start` changed would restart a running show.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const current = slides.current
  const position = useMemo<Position>(
    () => (current === null ? start : { roomId: current.room.id, index: current.index }),
    [current, start],
  )

  useEffect(() => {
    if (current === null) return
    // A new pace counts from the moment it is chosen, not from when the
    // picture went up.
    timer.current = window.setTimeout(() => void go(position, 1), pace * 1000)
    return () => window.clearTimeout(timer.current)
  }, [current, position, go, pace])

  // The controls and the cursor get out of the way while nobody is touching
  // anything, and come back on the first movement.
  // They stay up while the interval dropdown has focus, so they do not fade
  // out from under someone choosing.
  const sleep = useRef<number | undefined>(undefined)
  const paceSelect = useRef<HTMLSelectElement>(null)
  const wake = useCallback(() => {
    setAwake(true)
    window.clearTimeout(sleep.current)
    const doze = () => {
      if (paceSelect.current !== null && document.activeElement === paceSelect.current) {
        sleep.current = window.setTimeout(doze, CONTROLS_LINGER)
      } else {
        setAwake(false)
      }
    }
    sleep.current = window.setTimeout(doze, CONTROLS_LINGER)
  }, [])
  useEffect(() => {
    wake()
    return () => window.clearTimeout(sleep.current)
  }, [wake])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return
      // The dropdown's own arrows and Space choose an interval.
      if ((event.target as HTMLElement | null)?.closest('select')) return
      switch (event.key) {
        case 'ArrowRight':
          void go(position, 1)
          break
        case 'ArrowLeft':
          void go(position, -1)
          break
        case ' ':
        case 'p':
        case 'P':
          onStop(position)
          break
        case 'Escape':
          onExit(position)
          break
        default:
          return
      }
      event.preventDefault()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [go, position, onStop, onExit])

  const previous = slides.previous

  return (
    <div
      className={`slideshow${awake ? '' : ' slideshow--asleep'}`}
      role="dialog"
      aria-modal="true"
      aria-label="スライドショー"
      onPointerMove={wake}
      onPointerDown={wake}
    >
      <div className="slideshow__stage">
        {previous !== null && (
          <img
            key={previous.serial}
            className="slideshow__image slideshow__image--outgoing"
            src={previous.src}
            alt=""
            draggable={false}
          />
        )}
        {current !== null && (
          <img
            key={current.serial}
            className="slideshow__image slideshow__image--incoming"
            src={current.src}
            alt={current.image.name}
            draggable={false}
            onAnimationEnd={() => setSlides((shown) => ({ ...shown, previous: null }))}
          />
        )}
      </div>

      {error !== null && <p className="notice notice--error">{error}</p>}

      <div className="slideshow__controls">
        {current !== null && (
          <p className="lightbox__caption">
            {current.image.name}
            <span>
              {current.room.name} · {current.index + 1} / {current.room.images.length}
            </span>
          </p>
        )}
        <label className="slideshow__interval" title="切り替えの間隔">
          <Timer aria-hidden="true" />
          <select
            ref={paceSelect}
            value={pace}
            aria-label="切り替えの間隔"
            onChange={(event) => {
              choosePace(Number(event.currentTarget.value))
              // Let go of focus, so the controls can fade again and the keys
              // go back to the slideshow.
              event.currentTarget.blur()
            }}
          >
            {INTERVALS.map((seconds) => (
              <option key={seconds} value={seconds}>
                {intervalLabel(seconds)}
              </option>
            ))}
          </select>
        </label>
        <button
          type="button"
          className="lightbox__close lightbox__play"
          onClick={() => onStop(position)}
          aria-label="一時停止"
          aria-keyshortcuts="P"
          title="一時停止 (P)"
        >
          <Pause aria-hidden="true" />
        </button>
        <button
          type="button"
          className="lightbox__close"
          onClick={() => onExit(position)}
          aria-label="閉じる"
          title="閉じる (Esc)"
        >
          <X aria-hidden="true" />
        </button>
      </div>
    </div>
  )
}
