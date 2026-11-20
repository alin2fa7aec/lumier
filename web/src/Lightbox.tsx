import { useEffect } from 'react'

import { imageUrl, type Room } from './api'

/// One picture at full size.
///
/// Deliberately fetches `orig` rather than the largest rendition: this is the
/// one place the visitor has asked to see the actual file, and it is a single
/// deliberate request rather than a wall's worth.
export function Lightbox({
  room,
  index,
  onMove,
  onClose,
}: {
  room: Room
  index: number
  onMove: (index: number) => void
  onClose: () => void
}) {
  const image = room.images[index]

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return
      switch (event.key) {
        case 'ArrowRight':
          onMove(Math.min(index + 1, room.images.length - 1))
          break
        case 'ArrowLeft':
          onMove(Math.max(index - 1, 0))
          break
        case 'Escape':
          onClose()
          break
        default:
          return
      }
      event.preventDefault()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [index, room.images.length, onMove, onClose])

  if (image === undefined) return null

  return (
    <div
      className="lightbox"
      role="dialog"
      aria-modal="true"
      aria-label={image.name}
      onClick={onClose}
    >
      <img
        className="lightbox__image"
        src={imageUrl(image, 'orig')}
        alt={image.name}
        width={image.w}
        height={image.h}
        // The backdrop closes on click; the picture itself should not.
        onClick={(event) => event.stopPropagation()}
      />
      <p className="lightbox__caption">
        {image.name}
        <span>
          {image.w}×{image.h} · {index + 1} / {room.images.length}
        </span>
      </p>
      <button type="button" className="lightbox__close" onClick={onClose} aria-label="閉じる">
        ×
      </button>
    </div>
  )
}
