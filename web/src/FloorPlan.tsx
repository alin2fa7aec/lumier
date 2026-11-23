import { ChevronLeft, ChevronRight, MapPin } from 'lucide-react'
import { useEffect, useMemo, useRef, type Ref } from 'react'

import { imageUrl, MARKS_WALL, TRASH_WALL, type Museum, type MuseumRoom } from './api'
import { jumpToRoom } from './route'

/// The floor plan: every room, reachable in one step.
///
/// This is a way to move, not just a way to look. Doors alone make a deep graph
/// impractical — a twenty-room corridor puts its far end twenty doors away —
/// so every room here is one click from anywhere.
///
/// Rooms are laid out as the tree the doors make from the entrance: a column
/// per door walked, and each room's onward rooms in the next column, starting
/// on its own row and joined to it by a line. Rooms that share a column but
/// not a parent therefore sit in separate runs under separate lines, so a
/// branch reads as a branch rather than as one long flat column. It scrolls
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
  const plan = useMemo(() => layOut(museum), [museum])
  // The plan can be taller than the screen as well as wider, so the room you
  // are in is brought into view rather than left wherever it falls. Only as
  // far as it takes: a deep room then lands at the right edge, with the rooms
  // leading to it on screen to its left.
  const hereRef = useRef<HTMLButtonElement>(null)
  useEffect(() => {
    hereRef.current?.scrollIntoView({ block: 'nearest', inline: 'nearest' })
  }, [])
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

  const card = (room: MuseumRoom) => (
    <RoomCard
      key={room.id}
      ref={room.id === currentRoomId ? hereRef : undefined}
      room={room}
      here={room.id === currentRoomId}
      doorSides={room.id === currentRoomId ? undefined : adjacent.get(room.id)}
      onPick={() => {
        jumpToRoom(room.id)
        onClose()
      }}
    />
  )

  return (
    <div className="plan" role="dialog" aria-modal="true" aria-label="Floor plan" onClick={onClose}>
      <div className="plan__columns" onClick={(event) => event.stopPropagation()}>
        <div
          className="plan__tree"
          style={{ gridTemplateColumns: treeColumns(plan.depths) }}
        >
          {plan.links.map((link) => (
            <span
              key={`${link.depth}:${link.row}`}
              className={`plan__link plan__link--${link.kind}`}
              style={{ gridRow: link.row + 1, gridColumn: cardColumn(link.depth) - 1 }}
              aria-hidden="true"
            />
          ))}
          {plan.tree.map((node) => (
            // The line and the card share the row, so the line meets the card
            // at its middle whatever height the row grows to.
            <div
              key={node.room.id}
              className="plan__cell"
              style={{ gridRow: node.row + 1, gridColumn: cardColumn(node.depth) }}
            >
              {card(node.room)}
            </div>
          ))}
        </div>
        {plan.apart.map((column) => (
          <section className="plan__column" key={column.key}>
            {column.rooms.map(card)}
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
  ref,
  room,
  here,
  doorSides,
  onPick,
}: {
  ref?: Ref<HTMLButtonElement>
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
    <button
      ref={ref}
      type="button"
      className={`card card--${state}`}
      onClick={onPick}
      aria-current={here}
    >
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

/// A room's place in the tree.
type TreeNode = { room: MuseumRoom; depth: number; row: number }

/// One row's piece of the line from a parent to its onward rooms, in the gap
/// before their column: straight across for an only child, and for a run of
/// siblings, down from the first, past the middle ones, ending at the last.
/// `pass` carries the vertical stroke past rows taken by an earlier
/// sibling's own onward rooms, where no sibling stands.
type LinkKind = 'only' | 'first' | 'middle' | 'last' | 'pass'
type Link = { depth: number; row: number; kind: LinkKind }

/// Columns carry no heading: their order already says how far each room is,
/// and the visitor needs where to go, not the count of doors.
type Column = { key: string; rooms: MuseumRoom[] }

type Plan = {
  tree: TreeNode[]
  links: Link[]
  /// How many columns the tree takes.
  depths: number
  /// Rooms outside the tree, in columns of their own after it.
  apart: Column[]
}

/// Breadth-first from the entrance over directed door edges, since a door is
/// one-way unless the manifest made it reciprocal. Each room hangs under the
/// room that first reached it, so it appears once, at its shortest distance;
/// a room behind doors from two rooms shows under one of them.
///
/// Onward doors come before the ones leading back, and within an end in
/// manifest order, as in the slideshow's tour: the manifest's order of doors
/// is the curation.
function layOut(museum: Museum): Plan {
  const byId = new Map(museum.rooms.map((room) => [room.id, room]))
  const children = new Map<string, string[]>()
  const seen = new Set<string>([museum.entrance])
  const queue: string[] = [museum.entrance]

  while (queue.length > 0) {
    const id = queue.shift()
    if (id === undefined) break
    const room = byId.get(id)
    if (room === undefined) continue
    const kids: string[] = []
    for (const door of [...room.doors.right, ...room.doors.left]) {
      if (!seen.has(door.to) && byId.has(door.to)) {
        seen.add(door.to)
        kids.push(door.to)
        queue.push(door.to)
      }
    }
    children.set(id, kids)
  }

  // Depth first, so a room's subtree takes a contiguous run of rows that
  // starts on the room's own row; a room without onward rooms takes one.
  const tree: TreeNode[] = []
  const links: Link[] = []
  let nextRow = 0
  let depths = 0
  const place = (id: string, depth: number): number => {
    const room = byId.get(id)
    const row = nextRow
    if (room === undefined) return row
    depths = Math.max(depths, depth + 1)
    tree.push({ room, depth, row })
    const kids = children.get(id) ?? []
    if (kids.length === 0) nextRow++
    const rows = kids.map((kid) => place(kid, depth + 1))
    if (rows.length === 1) links.push({ depth: depth + 1, row: rows[0], kind: 'only' })
    if (rows.length > 1) {
      const first = rows[0]
      const last = rows[rows.length - 1]
      for (let r = first; r <= last; r++) {
        const kind = r === first ? 'first' : r === last ? 'last' : rows.includes(r) ? 'middle' : 'pass'
        links.push({ depth: depth + 1, row: r, kind })
      }
    }
    return row
  }
  if (byId.has(museum.entrance)) place(museum.entrance, 0)

  const apart: Column[] = []
  // `lumier check` warns about these, but the plan must still offer a way in:
  // a jump is the only way to reach them. They go after the tree.
  const unreachable = museum.rooms.filter((room) => !seen.has(room.id))
  if (unreachable.length > 0) apart.push({ key: 'unreachable', rooms: unreachable })

  // The walls for sorting out deletions stand apart from the museum, after
  // every room. They have no doors, so the plan is the way in.
  if (museum.marking) {
    apart.push({
      key: 'marking',
      rooms: [specialWall(MARKS_WALL, 'Marked for deletion'), specialWall(TRASH_WALL, 'Trash')],
    })
  }
  return { tree, links, depths, apart }
}

/// A card column per depth, with a narrow column between each pair for the
/// lines.
function treeColumns(depths: number): string {
  return Array.from({ length: depths }, () => 'var(--plan-card)').join(' var(--plan-link) ')
}

/// Grid lines for a card at `depth`: card columns are odd, and the gap for
/// the line into a column is the even one before it. Rows count from 1.
function cardColumn(depth: number): number {
  return depth * 2 + 1
}

function specialWall(id: string, name: string): MuseumRoom {
  return { id, name, image_count: 0, slideshow: false, cover: null, doors: { left: [], right: [] } }
}
