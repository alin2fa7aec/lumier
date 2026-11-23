import { ChevronLeft, ChevronRight, MapPin } from 'lucide-react'
import { useMemo } from 'react'

import { imageUrl, MARKS_WALL, TRASH_WALL, type Museum, type MuseumRoom } from './api'
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
  // Which end of the current wall leads to each neighbouring room. A room can
  // be behind doors at both ends.
  const adjacent = useMemo(() => {
    const sides = new Map<string, Sides>()
    const current = museum.rooms.find((room) => room.id === currentRoomId)
    if (current === undefined) return sides
    const mark = (to: string, side: keyof Sides) => {
      const entry = sides.get(to) ?? { left: false, right: false }
      entry[side] = true
      sides.set(to, entry)
    }
    for (const door of current.doors.left) mark(door.to, 'left')
    for (const door of current.doors.right) mark(door.to, 'right')
    return sides
  }, [museum, currentRoomId])

  return (
    <div className="plan" role="dialog" aria-modal="true" aria-label="Floor plan" onClick={onClose}>
      <div className="plan__columns" onClick={(event) => event.stopPropagation()}>
        {columns.map((column) => (
          <section className="plan__column" key={column.key}>
            {column.rooms.map((room) => (
              <RoomCard
                key={room.id}
                room={room}
                here={room.id === currentRoomId}
                doorSides={room.id === currentRoomId ? undefined : adjacent.get(room.id)}
                onPick={() => {
                  jumpToRoom(room.id)
                  onClose()
                }}
              />
            ))}
          </section>
        ))}
      </div>
      <button type="button" className="lightbox__close" onClick={onClose} aria-label="Close">
        ×
      </button>
    </div>
  )
}

type Sides = { left: boolean; right: boolean }

/// Where the room stands relative to the visitor is shown with marks rather
/// than words: a pin for the room you are in, and for a neighbour the same
/// chevrons the wall's doors use, pointing at the end of the current wall
/// whose door leads there. The words stay in the tooltip and accessible name.
function RoomCard({
  room,
  here,
  doorSides,
  onPick,
}: {
  room: MuseumRoom
  here: boolean
  doorSides: Sides | undefined
  onPick: () => void
}) {
  const state = here ? 'here' : doorSides !== undefined ? 'adjacent' : 'elsewhere'
  const doorLabel =
    doorSides === undefined
      ? ''
      : doorSides.left && doorSides.right
        ? 'Through the doors at both ends'
        : doorSides.left
          ? 'Through the door on the left'
          : 'Through the door on the right'
  return (
    <button type="button" className={`card card--${state}`} onClick={onPick} aria-current={here}>
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
      <span className="card__caption">
        <span className="card__name">{room.name}</span>
        {here && (
          <span className="card__mark" role="img" aria-label="You are here" title="You are here">
            <MapPin aria-hidden="true" />
          </span>
        )}
        {doorSides !== undefined && (
          <span className="card__mark" role="img" aria-label={doorLabel} title={doorLabel}>
            {doorSides.left && <ChevronLeft aria-hidden="true" />}
            {doorSides.right && <ChevronRight aria-hidden="true" />}
          </span>
        )}
      </span>
    </button>
  )
}

/// Columns carry no heading: their order already says how far each room is,
/// and the visitor needs where to go, not the count of doors.
type Column = { key: string; rooms: MuseumRoom[] }

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
    .map(([depth, rooms]) => ({ key: `depth-${depth}`, rooms }))

  // `lumier check` warns about these, but the plan must still offer a way in:
  // a jump is the only way to reach them. They go last.
  if (unreachable.length > 0) {
    columns.push({ key: 'unreachable', rooms: unreachable })
  }

  // The walls for sorting out deletions stand apart from the museum, after
  // every room. They have no doors, so the plan is the way in.
  if (museum.marking) {
    columns.push({
      key: 'marking',
      rooms: [specialWall(MARKS_WALL, 'Marked for deletion'), specialWall(TRASH_WALL, 'Trash')],
    })
  }
  return columns
}

function specialWall(id: string, name: string): MuseumRoom {
  return { id, name, image_count: 0, slideshow: false, cover: null, doors: { left: [], right: [] } }
}
