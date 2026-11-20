/// Remembers how far along each wall the visitor had walked.
///
/// Without this, every return trip through a door dumps you back at an end and
/// the museum stops feeling like a place. Kept per tab in `sessionStorage`: a
/// position is worth keeping for the length of a visit, not beyond it.
///
/// Every access is guarded — storage can be unavailable (private windows,
/// blocked site data) and these positions are a convenience, never something
/// to fail a render over.

const KEY = 'lumier:scroll'

type Positions = Record<string, number>

function read(): Positions {
  try {
    const stored = window.sessionStorage.getItem(KEY)
    if (stored === null) return {}
    const parsed: unknown = JSON.parse(stored)
    return typeof parsed === 'object' && parsed !== null ? (parsed as Positions) : {}
  } catch {
    return {}
  }
}

export function recall(roomId: string): number | null {
  const position = read()[roomId]
  return typeof position === 'number' && Number.isFinite(position) ? position : null
}

export function remember(roomId: string, scrollLeft: number): void {
  try {
    const positions = read()
    positions[roomId] = Math.round(scrollLeft)
    window.sessionStorage.setItem(KEY, JSON.stringify(positions))
  } catch {
    // Nothing to do and nothing worth reporting.
  }
}
