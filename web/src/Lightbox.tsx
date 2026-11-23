import { ExternalLink, Play, Trash2, Undo2 } from 'lucide-react'
import { useEffect, useRef, useState, type PointerEvent } from 'react'

import {
  fetchNote,
  formatDuration,
  imageUrl,
  linkedId,
  provenance,
  type ImageEntry,
  type Room,
} from './api'

/// How far a finger has to travel before the gesture commits to an axis. Below
/// this a touch is still a tap.
const AXIS_LOCK = 10
/// A sideways swipe this long moves to the neighbouring picture.
const STEP_DISTANCE = 60
/// A vertical swipe this long closes the lightbox.
const DISMISS_DISTANCE = 90
/// A flick this fast commits however short it was, in px per ms.
const FLICK_VELOCITY = 0.4
/// Past the first or last picture the image still follows, at this fraction of
/// the finger, so the end is felt rather than silently ignored.
const EDGE_RESISTANCE = 0.3
/// How long after a swipe a click is taken to be the swipe's own and ignored.
const CLICK_AFTER_SWIPE = 400

/// The one mark this picture can take: a deletion mark on a room's wall, a
/// restore request in the trash.
export type MarkControl = {
  kind: 'mark' | 'restore'
  onToggle: () => void
}

type Gesture = { pointer: number; x: number; y: number; at: number; axis: 'x' | 'y' | null }

/// Opens the source file in a new tab: a PDF in the browser's own viewer, a
/// picture at its full resolution.
function openOriginal(image: ImageEntry): void {
  window.open(imageUrl(image, 'orig'), '_blank', 'noopener')
}

/// A GIF is shown from its source: the renditions are stills, and would stop
/// an animation on its first frame.
function isGif(image: ImageEntry): boolean {
  return /\.gif$/i.test(image.name)
}

/// A note, rendered on the server, laid out for reading.
///
/// The HTML has already been sanitized there, which is what makes setting it
/// directly safe. Its links all open a new tab; one leading to something else
/// on this wall is caught here instead and moves the lightbox to it, so
/// reading a note and looking at what it describes stay in one place.
function NoteReader({
  image,
  room,
  onMove,
}: {
  image: ImageEntry
  room: Room
  onMove: (index: number) => void
}) {
  const [note, setNote] = useState<{ id: string; html: string } | null>(null)
  const [failed, setFailed] = useState<string | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    fetchNote(image, controller.signal)
      .then(({ html }) => setNote({ id: image.id, html }))
      .catch((cause: unknown) => {
        if (controller.signal.aborted) return
        setFailed(cause instanceof Error ? cause.message : String(cause))
      })
    return () => controller.abort()
  }, [image])

  const html = note !== null && note.id === image.id ? note.html : null
  return (
    <article
      className="lightbox__note"
      onClick={(event) => {
        event.stopPropagation()
        const anchor = (event.target as HTMLElement).closest('a')
        const id = anchor === null ? null : linkedId(anchor.getAttribute('href') ?? '')
        if (id === null) return
        const index = room.images.findIndex((entry) => entry.id === id)
        // A PDF still goes to the browser's viewer; anything else on this
        // wall opens here.
        if (index >= 0 && room.images[index].kind !== 'pdf') {
          event.preventDefault()
          onMove(index)
        }
      }}
    >
      {failed !== null ? (
        <p className="lightbox__note-error">{failed}</p>
      ) : html === null ? (
        <p className="lightbox__note-loading">Loading…</p>
      ) : (
        <div dangerouslySetInnerHTML={{ __html: html }} />
      )}
    </article>
  )
}

/// One picture at full size.
///
/// A picture is shown from its 2560 rendition, not the source file. A camera
/// JPEG runs to several megabytes, which a phone pays for every picture it
/// steps through, and a HEIC would not display outside Safari at all
/// (`docs/design.md` §6.6). The source stays one button (or Enter) away, in a
/// new tab, for looking at it at full resolution. A GIF keeps its source so
/// it still moves.
///
/// On touch screens a swipe sideways moves between pictures and a swipe up or
/// down closes, alongside the close button and the backdrop tap. The mouse is
/// left alone: dragging a picture on a desktop is not a gesture anyone expects.
export function Lightbox({
  room,
  index,
  onMove,
  onClose,
  onPlay,
  mark,
}: {
  room: Room
  index: number
  onMove: (index: number) => void
  onClose: () => void
  /// Starts the slideshow from this picture. Absent until the floor plan has
  /// loaded, since the slideshow walks the whole museum.
  onPlay?: () => void
  /// Absent when the server takes no marks.
  mark?: MarkControl
}) {
  const image = room.images[index]
  const last = room.images.length - 1

  const backdrop = useRef<HTMLDivElement>(null)
  // An <img> or, for a video, a <video>; set through a callback ref because
  // the two element types do not share a ref object type.
  const picture = useRef<HTMLElement | null>(null)
  const gesture = useRef<Gesture | null>(null)
  const ignoreClicksUntil = useRef(0)
  // Which side the incoming picture slides in from, so it arrives from where
  // the visitor pushed it.
  const [enterFrom, setEnterFrom] = useState<'left' | 'right' | null>(null)

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return
      switch (event.key) {
        case 'ArrowRight':
          setEnterFrom('right')
          onMove(Math.min(index + 1, last))
          break
        case 'ArrowLeft':
          setEnterFrom('left')
          onMove(Math.max(index - 1, 0))
          break
        case 'Escape':
          onClose()
          break
        case 'd':
        case 'D':
          if (mark === undefined) return
          mark.onToggle()
          break
        case 'Enter':
          if (image === undefined || (image.kind !== 'pdf' && image.kind !== 'image')) return
          openOriginal(image)
          break
        default:
          return
      }
      event.preventDefault()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [index, last, onMove, onClose, mark, image])

  // Styles are written straight onto the elements while a finger is down,
  // rather than through state, so following the finger never waits on a
  // render.
  const follow = (dx: number, dy: number, axis: 'x' | 'y') => {
    const img = picture.current
    const back = backdrop.current
    if (img === null || back === null) return
    img.style.transition = 'none'
    if (axis === 'x') {
      const pastEdge = (dx > 0 && index === 0) || (dx < 0 && index === last)
      img.style.transform = `translateX(${pastEdge ? dx * EDGE_RESISTANCE : dx}px)`
    } else {
      img.style.transform = `translateY(${dy}px)`
      const fade = Math.min(Math.abs(dy) / 300, 1) * 0.7
      back.style.backgroundColor = `rgb(11 11 13 / ${0.93 * (1 - fade)})`
    }
  }

  const settle = () => {
    const img = picture.current
    const back = backdrop.current
    if (img !== null) {
      img.style.transition = 'transform 200ms ease'
      img.style.transform = ''
    }
    if (back !== null) back.style.backgroundColor = ''
  }

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.pointerType === 'mouse') return
    if (gesture.current !== null) {
      // A second finger: this is a pinch, not a swipe.
      gesture.current = null
      settle()
      return
    }
    // A video's own controls take the touch: scrubbing must not page away.
    // A note scrolls and selects under the finger, so it keeps it too.
    if ((event.target as HTMLElement).closest('video, .lightbox__note') !== null) return
    // Zoomed in, a drag is for looking around the picture.
    if ((window.visualViewport?.scale ?? 1) > 1.01) return
    gesture.current = {
      pointer: event.pointerId,
      x: event.clientX,
      y: event.clientY,
      at: event.timeStamp,
      axis: null,
    }
  }

  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const current = gesture.current
    if (current === null || event.pointerId !== current.pointer) return
    const dx = event.clientX - current.x
    const dy = event.clientY - current.y
    if (current.axis === null) {
      if (Math.max(Math.abs(dx), Math.abs(dy)) < AXIS_LOCK) return
      current.axis = Math.abs(dx) > Math.abs(dy) ? 'x' : 'y'
      event.currentTarget.setPointerCapture(event.pointerId)
    }
    follow(dx, dy, current.axis)
  }

  const onPointerUp = (event: PointerEvent<HTMLDivElement>) => {
    const current = gesture.current
    if (current === null || event.pointerId !== current.pointer) return
    gesture.current = null
    // Never left the dead zone: a tap, which the click handlers deal with.
    if (current.axis === null) return

    ignoreClicksUntil.current = event.timeStamp + CLICK_AFTER_SWIPE
    const elapsed = Math.max(event.timeStamp - current.at, 1)

    if (current.axis === 'x') {
      const dx = event.clientX - current.x
      const committed = Math.abs(dx) > STEP_DISTANCE || Math.abs(dx) / elapsed > FLICK_VELOCITY
      const target = dx < 0 ? index + 1 : index - 1
      if (committed && target >= 0 && target <= last) {
        setEnterFrom(dx < 0 ? 'right' : 'left')
        onMove(target)
        return
      }
    } else {
      const dy = event.clientY - current.y
      if (Math.abs(dy) > DISMISS_DISTANCE || Math.abs(dy) / elapsed > FLICK_VELOCITY) {
        onClose()
        return
      }
    }
    settle()
  }

  const onPointerCancel = () => {
    gesture.current = null
    settle()
  }

  if (image === undefined) return null
  const pictureClass = `lightbox__image${enterFrom === null ? '' : ` lightbox__image--from-${enterFrom}`}${image.marked ? ' lightbox__image--marked' : ''}`
  const marked = mark?.kind === 'restore' ? Boolean(image.restore) : Boolean(image.marked)
  const markLabel =
    mark?.kind === 'restore'
      ? marked
        ? 'Keep in trash'
        : 'Restore'
      : marked
        ? 'Unmark'
        : 'Mark for deletion'
  const status = image.marked ? 'Marked for deletion' : image.restore ? 'To be restored' : null
  const origin = provenance(image)

  return (
    <div
      ref={backdrop}
      className="lightbox"
      role="dialog"
      aria-modal="true"
      aria-label={image.name}
      onClick={onClose}
      onClickCapture={(event) => {
        // The click a browser may still fire at the end of a swipe.
        if (event.timeStamp < ignoreClicksUntil.current) event.stopPropagation()
      }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerCancel}
    >
      {image.kind === 'markdown' ? (
        <NoteReader key={image.id} image={image} room={room} onMove={onMove} />
      ) : image.kind === 'pdf' ? (
        // The first page, large. Reading the document is the browser's job:
        // its viewer pages, searches and zooms, and drawing every page here
        // would mean shipping a PDF renderer to the client.
        <img
          ref={(element) => {
            picture.current = element
          }}
          key={image.id}
          className={`${pictureClass} lightbox__document`}
          src={imageUrl(image, 2560)}
          alt={image.name}
          width={image.w}
          height={image.h}
          draggable={false}
          title="Open the PDF (Enter)"
          onClick={(event) => {
            event.stopPropagation()
            openOriginal(image)
          }}
        />
      ) : image.kind === 'video' ? (
        <video
          ref={(element) => {
            picture.current = element
          }}
          key={image.id}
          className={pictureClass}
          src={imageUrl(image, 'orig')}
          width={image.w}
          height={image.h}
          controls
          autoPlay
          playsInline
          onClick={(event) => event.stopPropagation()}
        />
      ) : (
        <img
          ref={(element) => {
            picture.current = element
          }}
          // A fresh element per picture, so the entrance animation replays and
          // no transform from the swipe that brought it here lingers.
          key={image.id}
          className={pictureClass}
          src={imageUrl(image, isGif(image) ? 'orig' : 2560)}
          alt={image.name}
          width={image.w}
          height={image.h}
          draggable={false}
          // The backdrop closes on click; the picture itself should not.
          onClick={(event) => event.stopPropagation()}
        />
      )}
      <p className="lightbox__caption">
        {image.name}
        {(status !== null || origin !== null) && (
          <span className="lightbox__status">
            {[status, origin].filter((part) => part !== null).join(' · ')}
          </span>
        )}
        <span>
          {/* A note's size is only the shape of its card. */}
          {image.kind !== 'markdown' && `${image.w}×${image.h} · `}
          {image.duration !== undefined && `${formatDuration(image.duration)} · `}
          {index + 1} / {room.images.length}
        </span>
      </p>
      {onPlay !== undefined && (
        <button
          type="button"
          className="lightbox__close lightbox__play"
          onClick={(event) => {
            event.stopPropagation()
            onPlay()
          }}
          aria-label="Slideshow"
          aria-keyshortcuts="P"
          title="Slideshow (P)"
        >
          <Play aria-hidden="true" />
        </button>
      )}
      {(image.kind === 'pdf' || image.kind === 'image') && (
        <button
          type="button"
          className="lightbox__close lightbox__open"
          onClick={(event) => {
            event.stopPropagation()
            openOriginal(image)
          }}
          aria-label={image.kind === 'pdf' ? 'Open the PDF' : 'Open the original'}
          aria-keyshortcuts="Enter"
          title={image.kind === 'pdf' ? 'Open the PDF (Enter)' : 'Open the original (Enter)'}
        >
          <ExternalLink aria-hidden="true" />
        </button>
      )}
      {mark !== undefined && (
        <button
          type="button"
          className={`lightbox__close lightbox__mark${marked ? ' lightbox__mark--on' : ''}`}
          onClick={(event) => {
            event.stopPropagation()
            mark.onToggle()
          }}
          aria-pressed={marked}
          aria-label={markLabel}
          aria-keyshortcuts="D"
          title={`${markLabel} (D)`}
        >
          {mark.kind === 'restore' ? <Undo2 aria-hidden="true" /> : <Trash2 aria-hidden="true" />}
        </button>
      )}
      <button type="button" className="lightbox__close" onClick={onClose} aria-label="Close">
        ×
      </button>
    </div>
  )
}
