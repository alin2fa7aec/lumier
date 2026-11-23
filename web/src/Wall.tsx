import { ChevronLeft, ChevronRight, FileText, Play, Trash2, Undo2 } from 'lucide-react'
import { useEffect, useLayoutEffect, useMemo, useRef } from 'react'

import {
  formatDuration,
  imageUrl,
  noteTitle,
  pickRendition,
  provenance,
  type Door,
  type ImageEntry,
  type Room,
} from './api'
import { enterRoom, type ArriveAt } from './route'
import { recall, remember } from './scrollMemory'
import type { TierLayout } from './useTierLayout'

/// How far one arrow press moves, as a share of the viewport width.
const STEP = 0.3
/// Slack when deciding whether the wall is scrolled to an end. Fractional
/// device pixel ratios leave scrollLeft a hair short of scrollWidth.
const EDGE_SLACK = 4

/// One room rendered as a wall hung in tiers.
///
/// Each tier is a horizontal band, and each picture in turn goes into the
/// band that is shortest so far, so pictures of every shape pack edge to edge
/// with only the right-hand end left ragged. The order along the wall is
/// therefore roughly, not strictly, left to right. The only scrolling is
/// still sideways: reaching either end simply stops, and the doors stand at
/// the two ends. How many tiers there are follows from the viewport, decided
/// in CSS.
export function Wall({
  room,
  arriveAt,
  tiers,
  keyboard,
  selecting,
  onOpenImage,
}: {
  room: Room
  arriveAt: ArriveAt
  tiers: TierLayout
  /// False while an overlay is up, so arrow keys do not scroll the wall
  /// underneath it.
  keyboard: boolean
  /// While selecting, a tap marks a picture instead of opening it, so a wall
  /// can be sorted through without a trip into the lightbox for each.
  selecting: boolean
  onOpenImage: (index: number) => void
}) {
  const wall = useRef<HTMLDivElement>(null)
  const bands = useMemo(() => pack(room.images, tiers), [room.images, tiers])
  const tierHeight = tiers.height

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
    <div
      className={`wall${selecting ? ' wall--selecting' : ''}`}
      ref={wall}
      role="region"
      aria-label={`${room.name} wall`}
    >
      <DoorGroup doors={room.doors.left} side="left" />
      {/* The tiers are a grid of their own, so where the doors sit does not
          depend on how the pictures happen to flow into it. */}
      <div className="tiers">
        {bands.map((band, bandIndex) => (
          <div className="band" key={bandIndex}>
            {band.map((index) => {
              const image = room.images[index]
              return (
                <figure
                  className={`frame${image.marked ? ' frame--marked' : ''}${image.restore ? ' frame--restore' : ''}`}
                  key={image.id}
                >
                  <button
                    type="button"
                    className="frame__button"
                    onClick={() => onOpenImage(index)}
                    title={provenance(image) ?? undefined}
                    aria-pressed={selecting ? Boolean(image.marked || image.restore) : undefined}
                    aria-label={
                      selecting
                        ? `Toggle mark on ${image.name}`
                        : `${image.kind === 'video' ? 'Play' : image.kind === 'markdown' ? 'Read' : 'Open'} ${image.name}`
                    }
                  >
                    {image.kind === 'video' ? (
                      <VideoFrame image={image} />
                    ) : image.kind === 'markdown' ? (
                      <NoteCard image={image} />
                    ) : (
                      <img
                        src={imageUrl(image, pickRendition(image, tierHeight, window.devicePixelRatio))}
                        alt={image.name}
                        width={image.w}
                        height={image.h}
                        loading="lazy"
                        decoding="async"
                        draggable={false}
                      />
                    )}
                    {image.kind === 'pdf' && (
                      <span className="frame__badge frame__badge--outline" aria-hidden="true">
                        <FileText />
                        PDF
                      </span>
                    )}
                    {image.marked && (
                      <span className="frame__mark" role="img" aria-label="Marked for deletion">
                        <Trash2 aria-hidden="true" />
                      </span>
                    )}
                    {image.restore && (
                      <span className="frame__mark" role="img" aria-label="To be restored">
                        <Undo2 aria-hidden="true" />
                      </span>
                    )}
                  </button>
                </figure>
              )
            })}
          </div>
        ))}
      </div>
      <DoorGroup doors={room.doors.right} side="right" />
    </div>
  )
}

/// Which pictures go in which band: each in turn into the band that is
/// shortest so far, measured as drawn widths plus the gap after each. The
/// first band wins a tie, so a short room fills from the top.
function pack(images: ImageEntry[], layout: TierLayout): number[][] {
  const bands = Array.from({ length: layout.tiers }, () => ({ length: 0, items: [] as number[] }))
  images.forEach((image, index) => {
    const aspect = image.h > 0 ? image.w / image.h : 1
    let shortest = bands[0]
    for (const band of bands) {
      if (band.length < shortest.length) shortest = band
    }
    shortest.items.push(index)
    shortest.length += layout.height * aspect + layout.gap
  })
  return bands.map((band) => band.items)
}

/// A note as it hangs on the wall: a portrait card with its title and
/// opening lines. Its shape comes from the `w`/`h` the server gives every
/// note, so it packs into the bands like a picture of that shape.
function NoteCard({ image }: { image: ImageEntry }) {
  return (
    <span className="frame__note" style={{ aspectRatio: `${image.w} / ${image.h}` }}>
      <span className="frame__note-title">{noteTitle(image)}</span>
      {image.excerpt !== undefined && image.excerpt !== '' && (
        <span className="frame__note-excerpt">{image.excerpt}</span>
      )}
    </span>
  )
}

/// A video as it hangs on the wall: its opening frame, never playing, with a
/// play mark and its length so it is not mistaken for a photo.
///
/// The browser draws the frame from the source itself — `preload="metadata"`
/// plus a media fragment fetches the header and one keyframe by Range, not
/// the whole file. There is no server-side thumbnail because rendering one
/// needs a video decoder, and the server deliberately has none.
function VideoFrame({ image }: { image: ImageEntry }) {
  return (
    <span className="frame__video">
      <video
        src={`${imageUrl(image, 'orig')}#t=0.1`}
        width={image.w}
        height={image.h}
        preload="metadata"
        muted
        playsInline
        disablePictureInPicture
        tabIndex={-1}
        aria-hidden="true"
      />
      <span className="frame__badge">
        <Play aria-hidden="true" />
        {image.duration !== undefined && formatDuration(image.duration)}
      </span>
    </span>
  )
}

/// The doors standing at one end of a wall. An end holds as many as the
/// manifest gives it, which is what lets a plain horizontal strip express a
/// branching graph.
function DoorGroup({ doors, side }: { doors: Door[]; side: 'left' | 'right' }) {
  if (doors.length === 0) return null
  return (
    <nav className={`doors doors--${side}`} aria-label={side === 'left' ? 'Doors on the left' : 'Doors on the right'}>
      {doors.map((door) => (
        <button
          type="button"
          className="door"
          key={door.to}
          onClick={() => enterRoom(door.to, side === 'right' ? 'start' : 'end')}
        >
          {side === 'left' && (
            <ChevronLeft className="door__arrow" aria-hidden="true" />
          )}
          <span className="door__name">{door.name}</span>
          {side === 'right' && (
            <ChevronRight className="door__arrow" aria-hidden="true" />
          )}
        </button>
      ))}
    </nav>
  )
}
