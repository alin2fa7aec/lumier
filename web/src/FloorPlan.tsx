import { useMemo } from 'react'

import { imageUrl, type Museum, type MuseumRoom } from './api'
import { jumpToRoom } from './route'

/// The floor plan: every room, reachable in one step.
///
/// This is a way to move, not just a way to look. Doors alone make a deep graph
/// impractical — a twenty-room corridor puts its far end twenty doors away —
/// so every room here is one click from anywhere.
///
/// Rooms are grouped by how many doors they stand from the entrance. That shows
/// the shape of the museum without drawing a graph: a corridor comes out as a
/// long row of single-room columns, a hub as one tall column. It scrolls
/// sideways, like everything else here.
export function FloorPlan({
  museum,
  currentRoomId,
  onClose,
}: {
  museum: Museum
  currentRoomId: string
  onClose: () => void
}) {
  const columns = useMemo(() => groupByDepth(museum), [museum])
  const adjacent = useMemo(() => {
    const current = museum.rooms.find((room) => room.id === currentRoomId)
    if (current === undefined) return new Set<string>()
    return new Set([...current.doors.left, ...current.doors.right].map((door) => door.to))
  }, [museum, currentRoomId])

  return (
    <div className="plan" role="dialog" aria-modal="true" aria-label="見取り図" onClick={onClose}>
      <div className="plan__columns" onClick={(event) => event.stopPropagation()}>
        {columns.map((column) => (
          <section className="plan__column" key={column.label}>
            <h2 className="plan__depth">{column.label}</h2>
            {column.rooms.map((room) => (
              <RoomCard
                key={room.id}
                room={room}
                state={
                  room.id === currentRoomId
                    ? 'here'
                    : adjacent.has(room.id)
                      ? 'adjacent'
                      : 'elsewhere'
                }
                onPick={() => {
                  jumpToRoom(room.id)
                  onClose()
                }}
              />
            ))}
          </section>
        ))}
      </div>
      <button type="button" className="lightbox__close" onClick={onClose} aria-label="閉じる">
        ×
      </button>
    </div>
  )
}

function RoomCard({
  room,
  state,
  onPick,
}: {
  room: MuseumRoom
  state: 'here' | 'adjacent' | 'elsewhere'
  onPick: () => void
}) {
  return (
    <button type="button" className={`card card--${state}`} onClick={onPick} aria-current={state === 'here'}>
      {room.cover !== null ? (
        <img
          className="card__cover"
          src={imageUrl(room.cover, 320)}
          alt=""
          loading="lazy"
          decoding="async"
          draggable={false}
        />
      ) : (
        <span className="card__cover card__cover--empty" aria-hidden="true" />
      )}
      <span className="card__name">{room.name}</span>
      <span className="card__meta">
        {state === 'here' && <span className="card__badge">現在地</span>}
        {state === 'adjacent' && <span className="card__badge card__badge--near">扉の先</span>}
        {room.image_count > 0 ? `${room.image_count} 点` : '通路'}
      </span>
    </button>
  )
}

type Column = { label: string; rooms: MuseumRoom[] }

/// Breadth-first from the entrance over directed door edges, since a door is
/// one-way unless the manifest made it reciprocal.
function groupByDepth(museum: Museum): Column[] {
  const byId = new Map(museum.rooms.map((room) => [room.id, room]))
  const depths = new Map<string, number>([[museum.entrance, 0]])
  const queue: string[] = [museum.entrance]

  while (queue.length > 0) {
    const id = queue.shift()
    if (id === undefined) break
    const room = byId.get(id)
    const depth = depths.get(id)
    if (room === undefined || depth === undefined) continue
    for (const door of [...room.doors.left, ...room.doors.right]) {
      if (!depths.has(door.to)) {
        depths.set(door.to, depth + 1)
        queue.push(door.to)
      }
    }
  }

  const byDepth = new Map<number, MuseumRoom[]>()
  const unreachable: MuseumRoom[] = []
  for (const room of museum.rooms) {
    const depth = depths.get(room.id)
    if (depth === undefined) {
      unreachable.push(room)
      continue
    }
    const bucket = byDepth.get(depth)
    if (bucket === undefined) byDepth.set(depth, [room])
    else bucket.push(room)
  }

  const columns: Column[] = [...byDepth.entries()]
    .sort(([a], [b]) => a - b)
    .map(([depth, rooms]) => ({
      label: depth === 0 ? 'エントランス' : `${depth} 手`,
      rooms,
    }))

  // `lumier check` warns about these, but the plan must still offer a way in:
  // a jump is the only way to reach them.
  if (unreachable.length > 0) {
    columns.push({ label: '扉では到達不可', rooms: unreachable })
  }
  return columns
}
