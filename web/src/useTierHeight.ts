import { useLayoutEffect, useState } from 'react'

/// How tall one tier of the wall is, in CSS pixels.
///
/// The value itself lives in CSS as `--tier-height`, derived there from the
/// wall height and the tier count (which varies with the viewport). This
/// measures it rather than restating it, so the layout has one source of truth.
/// A hidden probe element is the way to read a resolved CSS length back out —
/// `getComputedStyle` on a custom property hands back the unevaluated token
/// stream, not a pixel value.
///
/// Null until the first measurement, which happens before paint.
export function useTierHeight(): number | null {
  const [height, setHeight] = useState<number | null>(null)

  useLayoutEffect(() => {
    const probe = document.createElement('div')
    probe.className = 'tier-probe'
    document.body.append(probe)

    const measure = () => setHeight(probe.getBoundingClientRect().height)
    measure()
    // Catches a plain resize and a change of tier count at a media-query break
    // alike, since both change the probe's height.
    const observer = new ResizeObserver(measure)
    observer.observe(probe)

    return () => {
      observer.disconnect()
      probe.remove()
    }
  }, [])

  return height
}
