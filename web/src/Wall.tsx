import { useEffect, useLayoutEffect, useRef } from 'react'

import { imageUrl, pickRendition, type Door, type Room } from './api'
import { enterRoom, type ArriveAt } from './route'
import { recall, remember } from './scrollMemory'

/// How far one arrow press moves, as a share of the viewport width.
const STEP = 0.3
/// Slack when deciding whether the wall is scrolled to an end. Fractional
/// device pixel ratios leave scrollLeft a hair short of scrollWidth.
const EDGE_SLACK = 4

/// One room rendered as a wall hung in tiers.
///
/// Pictures fill a column top to bottom and then carry on rightwards, the way a
/// salon hangs them, so the only scrolling is still sideways: reaching either
/// end simply stops, and the doors stand at the two ends. How many tiers there
/// are follows from the viewport height, decided in CSS.
export function Wall({
  room,
  arriveAt,
  tierHeight,
  keyboard,
  onOpenImage,
}: {
  room: Room
  arriveAt: ArriveAt
  tierHeight: number
  /// False while an overlay is up, so arrow keys do not scroll the wall
  /// underneath it.
  keyboard: boolean
  onOpenImage: (index: number) => void
}) {
  const wall = useRef<HTMLDivElement>(null)

  // Place the visitor. One assignment is enough: every image carries its
  // dimensions, so the wall's full width is already laid out here, before
  // anything has been fetched.
  useLayoutEffect(() => {
    const element = wall.current
    if (element === null) return
    const saved = arriveAt === 'restore' ? recall(room.id) : null
    element.scrollLeft = saved ?? (arriveAt === 'end' ? element.scrollWidth : 0)
  }, [room.id, arriveAt])

  // Keep the position current, and write it once more on the way out so a
  // navigation never loses the last few pixels of movement.
  useEffect(() => {
    const element = wall.current
    if (element === null) return
    let pending: number | undefined
    const onScroll = () => {
      window.clearTimeout(pending)
      pending = window.setTimeout(() => remember(room.id, element.scrollLeft), 150)
    }
    element.addEventListener('scroll', onScroll, { passive: true })
    return () => {
      window.clearTimeout(pending)
      element.removeEventListener('scroll', onScroll)
      remember(room.id, element.scrollLeft)
    }
  }, [room.id])

  useEffect(() => {
    if (!keyboard) return
    const onKeyDown = (event: KeyboardEvent) => {
      const element = wall.current
      if (element === null) return
      if (event.metaKey || event.ctrlKey || event.altKey) return
      // A focused door owns its own Enter and arrow handling.
      if ((event.target as HTMLElement | null)?.closest('button, a, input, textarea, select')) {
        return
      }

      const step = Math.round(window.innerWidth * STEP)
      switch (event.key) {
        case 'ArrowRight':
          element.scrollBy({ left: step, behavior: 'smooth' })
          break
        case 'ArrowLeft':
          element.scrollBy({ left: -step, behavior: 'smooth' })
          break
        case 'Home':
          element.scrollTo({ left: 0, behavior: 'smooth' })
          break
        case 'End':
          element.scrollTo({ left: element.scrollWidth, behavior: 'smooth' })
          break
        case 'Enter': {
          // Standing at an end, Enter walks through. The forward direction wins
          // when the wall is short enough to be at both ends at once.
          const atEnd =
            element.scrollLeft + element.clientWidth >= element.scrollWidth - EDGE_SLACK
          const atStart = element.scrollLeft <= EDGE_SLACK
          const onward = atEnd ? room.doors.right[0] : undefined
          const back = atStart ? room.doors.left[0] : undefined
          if (onward !== undefined) {
            enterRoom(onward.to, 'start')
          } else if (back !== undefined) {
            enterRoom(back.to, 'end')
          } else {
            return
          }
          break
        }
        default:
          return
      }
      event.preventDefault()
    }

    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [keyboard, room])

  return (
    <div className="wall" ref={wall} role="region" aria-label={`${room.name} の壁`}>
      <DoorGroup doors={room.doors.left} side="left" />
      {/* The tiers are a grid of their own, so where the doors sit does not
          depend on how the pictures happen to flow into it. */}
      <div className="tiers">
        {room.images.map((image, index) => (
          <figure className="frame" key={image.id}>
            <button
              type="button"
              className="frame__button"
              onClick={() => onOpenImage(index)}
              aria-label={`${image.name} を拡大`}
            >
              <img
                src={imageUrl(image, pickRendition(image, tierHeight, window.devicePixelRatio))}
                alt={image.name}
                width={image.w}
                height={image.h}
                loading="lazy"
                decoding="async"
                draggable={false}
              />
            </button>
          </figure>
        ))}
      </div>
      <DoorGroup doors={room.doors.right} side="right" />
    </div>
  )
}

/// The doors standing at one end of a wall. An end holds as many as the
/// manifest gives it, which is what lets a plain horizontal strip express a
/// branching graph.
function DoorGroup({ doors, side }: { doors: Door[]; side: 'left' | 'right' }) {
  if (doors.length === 0) return null
  return (
    <nav className={`doors doors--${side}`} aria-label={side === 'left' ? '左の扉' : '右の扉'}>
      {doors.map((door) => (
        <button
          type="button"
          className="door"
          key={door.to}
          onClick={() => enterRoom(door.to, side === 'right' ? 'start' : 'end')}
        >
          {door.peek !== null && (
            // A glimpse of the far side. The smallest rendition is plenty —
            // it sits behind the plaque and is cropped to the opening.
            <img
              className="door__peek"
              src={imageUrl(door.peek, 320)}
              alt=""
              loading="lazy"
              decoding="async"
              draggable={false}
            />
          )}
          <span className="door__arrow" aria-hidden="true">
            {side === 'right' ? '→' : '←'}
          </span>
          <span className="door__name">{door.name}</span>
        </button>
      ))}
    </nav>
  )
}
