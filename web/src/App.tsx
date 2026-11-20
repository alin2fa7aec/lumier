import { useCallback, useEffect, useState } from 'react'

import { fetchMuseum, fetchRoom, type Museum, type Room } from './api'
import { FloorPlan } from './FloorPlan'
import { Lightbox } from './Lightbox'
import { replaceRoom, useRoute } from './route'
import { useTierHeight } from './useTierHeight'
import { Wall } from './Wall'

/// What is covering the wall, if anything. Only one at a time, which is also
/// what decides whether the wall still answers arrow keys.
///
/// An open picture records the room it came from, so leaving that room (the
/// back button, say) drops it during render rather than needing an effect to
/// clear it.
type Overlay = { kind: 'plan' } | { kind: 'image'; roomId: string; index: number } | null

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
  // What is actually on screen, after dropping a picture left behind by a
  // navigation. The wall keeps its keys whenever nothing is covering it.
  const showing =
    overlay?.kind === 'image' && overlay.roomId !== room?.id ? null : overlay

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
        <p>
          {room.images.length > 0 ? `${room.images.length} 点` : '通路'}
          {museum !== null && ` · 全 ${museum.rooms.length} 室`}
        </p>
      </header>

      <button
        type="button"
        className="plan-button"
        onClick={() => setOverlay({ kind: 'plan' })}
        // The shortcut is the quick way in; the button is how anyone finds it.
      >
        見取り図 <kbd>M</kbd>
      </button>

      {showing?.kind === 'plan' && museum !== null && (
        <FloorPlan museum={museum} currentRoomId={room.id} onClose={closeOverlay} />
      )}
      {showing?.kind === 'image' && (
        <Lightbox
          room={room}
          index={showing.index}
          onMove={(index) => setOverlay({ kind: 'image', roomId: room.id, index })}
          onClose={closeOverlay}
        />
      )}
    </>
  )
}
