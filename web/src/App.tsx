// Renamed so it does not shadow the global Map.
import { Map as MapIcon, Play } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'

import { fetchMuseum, fetchRoom, type Museum, type Room } from './api'
import { FloorPlan } from './FloorPlan'
import { Lightbox } from './Lightbox'
import { jumpToRoom, replaceRoom, useRoute } from './route'
import { Slideshow, type Position } from './Slideshow'
import { useTierHeight } from './useTierHeight'
import { Wall } from './Wall'

/// What is covering the wall, if anything. Only one at a time, which is also
/// what decides whether the wall still answers arrow keys.
///
/// An open picture records the room it came from, so leaving that room (the
/// back button, say) drops it during render rather than needing an effect to
/// clear it. The slideshow is the exception: it walks the whole museum, so it
/// is tied to no room and outlives a navigation underneath it.
type Overlay =
  | { kind: 'plan' }
  | { kind: 'image'; roomId: string; index: number }
  | { kind: 'slideshow'; start: Position }
  | null

export default function App() {
  const route = useRoute()
  const tierHeight = useTierHeight()
  const [museum, setMuseum] = useState<Museum | null>(null)
  // The room is stored with the id it was fetched for, so staleness after a
  // navigation is derived during render rather than cleared from an effect.
  const [loaded, setLoaded] = useState<{ roomId: string; room: Room } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [overlay, setOverlay] = useState<Overlay>(null)

  useEffect(() => {
    const controller = new AbortController()
    fetchMuseum(controller.signal)
      .then(setMuseum)
      .catch((cause: unknown) => {
        if (controller.signal.aborted) return
        setError(cause instanceof Error ? cause.message : String(cause))
      })
    return () => controller.abort()
  }, [])

  // A bare URL lands at the entrance, replacing rather than pushing so the
  // back button does not stall on a room the visitor never saw.
  useEffect(() => {
    if (museum !== null && route.roomId === null) {
      replaceRoom(museum.entrance)
    }
  }, [museum, route.roomId])

  useEffect(() => {
    const roomId = route.roomId
    if (roomId === null) return
    const controller = new AbortController()
    fetchRoom(roomId, controller.signal)
      .then((room) => setLoaded({ roomId, room }))
      .catch((cause: unknown) => {
        if (controller.signal.aborted) return
        setError(cause instanceof Error ? cause.message : String(cause))
      })
    return () => controller.abort()
  }, [route.roomId])

  const closeOverlay = useCallback(() => setOverlay(null), [])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return
      if ((event.target as HTMLElement | null)?.closest('input, textarea, select')) return
      if (event.key === 'm' || event.key === 'M') {
        setOverlay((current) => (current?.kind === 'plan' ? null : { kind: 'plan' }))
        event.preventDefault()
      } else if (event.key === 'Escape') {
        setOverlay(null)
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [])

  const room = loaded !== null && loaded.roomId === route.roomId ? loaded.room : null
  const playable = museum !== null && museum.rooms.some((entry) => entry.image_count > 0)
  // What is actually on screen, after dropping a picture left behind by a
  // navigation. The wall keeps its keys whenever nothing is covering it.
  const showing =
    overlay?.kind === 'image' && overlay.roomId !== room?.id ? null : overlay

  // Where P would start the slideshow: the open picture, or the start of the
  // room when only the wall is showing. Null while the floor plan is up, and
  // while the slideshow runs, since it then owns the key itself — two
  // listeners answering one press would stop the show and restart it.
  const playFrom: Position | null =
    !playable || room === null
      ? null
      : showing === null
        ? { roomId: room.id, index: 0 }
        : showing.kind === 'image'
          ? { roomId: showing.roomId, index: showing.index }
          : null
  const playRoomId = playFrom?.roomId
  const playIndex = playFrom?.index

  useEffect(() => {
    if (playRoomId === undefined || playIndex === undefined) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return
      if (event.key !== 'p' && event.key !== 'P') return
      if ((event.target as HTMLElement | null)?.closest('input, textarea, select')) return
      setOverlay({ kind: 'slideshow', start: { roomId: playRoomId, index: playIndex } })
      event.preventDefault()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [playRoomId, playIndex])

  // Leaving the slideshow puts the visitor in the room of the picture it
  // stopped on, which is usually not the room it started from.
  const leaveSlideshow = useCallback(
    (at: Position, reopen: boolean) => {
      if (at.roomId !== route.roomId) jumpToRoom(at.roomId)
      setOverlay(reopen ? { kind: 'image', roomId: at.roomId, index: at.index } : null)
    },
    [route.roomId],
  )
  const stopSlideshow = useCallback((at: Position) => leaveSlideshow(at, true), [leaveSlideshow])
  const exitSlideshow = useCallback((at: Position) => leaveSlideshow(at, false), [leaveSlideshow])

  if (error !== null) {
    return <p className="notice notice--error">{error}</p>
  }
  // The tier height decides which rendition each picture needs, so the wall
  // waits for the first measurement rather than fetching at a guessed size.
  // That measurement happens before paint, so nothing is visibly delayed.
  if (room === null || tierHeight === null) {
    return <p className="notice">読み込み中…</p>
  }

  return (
    <>
      <Wall
        room={room}
        arriveAt={route.arriveAt}
        tierHeight={tierHeight}
        keyboard={showing === null}
        onOpenImage={(index) => setOverlay({ kind: 'image', roomId: room.id, index })}
      />

      <header className="plaque">
        <h1>{room.name}</h1>
      </header>

      <button
        type="button"
        className="plan-button"
        onClick={() => setOverlay({ kind: 'plan' })}
        // The shortcut is the quick way in; the button is how anyone finds it.
        // A map icon alone says what it opens; the name and the shortcut live
        // in the tooltip and the accessible name.
        aria-label="見取り図"
        aria-keyshortcuts="M"
        title="見取り図 (M)"
      >
        <MapIcon aria-hidden="true" />
      </button>

      {playable && (
        <button
          type="button"
          className="plan-button play-button"
          onClick={() => setOverlay({ kind: 'slideshow', start: { roomId: room.id, index: 0 } })}
          aria-label="スライドショー"
          aria-keyshortcuts="P"
          title="スライドショー (P)"
        >
          <Play aria-hidden="true" />
        </button>
      )}

      {showing?.kind === 'plan' && museum !== null && (
        <FloorPlan museum={museum} currentRoomId={room.id} onClose={closeOverlay} />
      )}
      {showing?.kind === 'image' && (
        <Lightbox
          room={room}
          index={showing.index}
          onMove={(index) => setOverlay({ kind: 'image', roomId: room.id, index })}
          onClose={closeOverlay}
          onPlay={
            playable
              ? () =>
                  setOverlay({ kind: 'slideshow', start: { roomId: room.id, index: showing.index } })
              : undefined
          }
        />
      )}
      {showing?.kind === 'slideshow' && museum !== null && (
        <Slideshow
          museum={museum}
          start={showing.start}
          onStop={stopSlideshow}
          onExit={exitSlideshow}
        />
      )}
    </>
  )
}
