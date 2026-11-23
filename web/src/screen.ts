import { useCallback, useEffect, useRef, useState } from 'react'

/// Keeps the screen on while `active`, so a photo frame does not go dark
/// halfway through a slideshow.
///
/// The Wake Lock API exists only in a secure context: HTTPS, or localhost.
/// Opened over plain HTTP at a LAN address, `navigator.wakeLock` is simply
/// absent, and this does nothing — the screen then sleeps on the device's
/// own timer. A lock is also dropped whenever the page is hidden, so it is
/// asked for again each time the page comes back into view.
export function useWakeLock(active: boolean): void {
  useEffect(() => {
    if (!active || !('wakeLock' in navigator)) return
    let sentinel: WakeLockSentinel | null = null
    let cancelled = false

    const acquire = () => {
      if (document.visibilityState !== 'visible') return
      navigator.wakeLock
        .request('screen')
        .then((lock) => {
          if (cancelled) void lock.release()
          else sentinel = lock
        })
        // Refused, say, on low battery: the show carries on regardless.
        .catch(() => undefined)
    }
    acquire()
    document.addEventListener('visibilitychange', acquire)

    return () => {
      cancelled = true
      document.removeEventListener('visibilitychange', acquire)
      void sentinel?.release()
    }
  }, [active])
}

/// Whether the page is in fullscreen, and a way to toggle it.
///
/// `supported` is false where the browser offers no element fullscreen, as on
/// an iPhone, so the control can be left out rather than do nothing.
/// Fullscreen entered here is left again on unmount, so closing the slideshow
/// does not strand the visitor in a fullscreen wall.
export function useFullscreen(): { supported: boolean; on: boolean; toggle: () => void } {
  const supported = typeof document !== 'undefined' && document.fullscreenEnabled === true
  const [on, setOn] = useState(() => document.fullscreenElement !== null)
  const enteredHere = useRef(false)

  useEffect(() => {
    const onChange = () => {
      const now = document.fullscreenElement !== null
      setOn(now)
      if (!now) enteredHere.current = false
    }
    document.addEventListener('fullscreenchange', onChange)
    return () => {
      document.removeEventListener('fullscreenchange', onChange)
      if (enteredHere.current && document.fullscreenElement !== null) {
        void document.exitFullscreen().catch(() => undefined)
      }
    }
  }, [])

  const toggle = useCallback(() => {
    if (!supported) return
    if (document.fullscreenElement !== null) {
      void document.exitFullscreen().catch(() => undefined)
    } else {
      document.documentElement
        .requestFullscreen()
        .then(() => {
          enteredHere.current = true
        })
        .catch(() => undefined)
    }
  }, [supported])

  return { supported, on, toggle }
}
