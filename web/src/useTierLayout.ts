import { useLayoutEffect, useState } from 'react'

/// The shape of the wall, in CSS pixels: how many tiers, how tall each is,
/// and the gap between neighbouring pictures and tiers.
export type TierLayout = {
  tiers: number
  height: number
  gap: number
}

/// Measures the wall's tiers.
///
/// The values themselves live in CSS as `--tiers`, `--tier-height` and
/// `--tier-gap`, with the tier count varying by viewport. This measures them
/// rather than restating them, so the layout has one source of truth. A
/// hidden probe element is the way to read a resolved CSS length back out —
/// `getComputedStyle` on a custom property hands back the unevaluated token
/// stream, not a pixel value — so the probe is as tall as a tier, as many
/// pixels wide as there are tiers, and has the gap as its margin.
///
/// Null until the first measurement, which happens before paint.
export function useTierLayout(): TierLayout | null {
  const [layout, setLayout] = useState<TierLayout | null>(null)

  useLayoutEffect(() => {
    const probe = document.createElement('div')
    probe.className = 'tier-probe'
    document.body.append(probe)

    const measure = () => {
      const box = probe.getBoundingClientRect()
      const next = {
        tiers: Math.max(1, Math.round(box.width)),
        height: box.height,
        gap: Number.parseFloat(getComputedStyle(probe).marginLeft) || 0,
      }
      // A fresh object on every resize would repack the wall for nothing.
      setLayout((current) =>
        current !== null &&
        current.tiers === next.tiers &&
        current.height === next.height &&
        current.gap === next.gap
          ? current
          : next,
      )
    }
    measure()
    // Catches a plain resize and a change of tier count at a media-query break
    // alike, since both change the probe's size.
    const observer = new ResizeObserver(measure)
    observer.observe(probe)

    return () => {
      observer.disconnect()
      probe.remove()
    }
  }, [])

  return layout
}
