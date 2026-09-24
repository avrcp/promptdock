import { onBeforeUnmount } from 'vue'

// Overlays can nest (for example a destructive confirmation above a drawer).
// Keep a process-wide reference count so closing the inner overlay never
// releases the outer overlay's body-scroll lock.
let lockCount = 0
let previousOverflow = ''

function lockBodyScroll(): void {
  if (typeof document === 'undefined') return
  if (lockCount === 0) previousOverflow = document.body.style.overflow
  lockCount += 1
  document.body.style.overflow = 'hidden'
}

function unlockBodyScroll(): void {
  if (typeof document === 'undefined' || lockCount === 0) return
  lockCount -= 1
  if (lockCount === 0) document.body.style.overflow = previousOverflow
}

/** Per-component handle for the shared, reference-counted overlay scroll lock. */
export function useOverlayScrollLock(): { lock: () => void; unlock: () => void } {
  let locked = false

  function lock(): void {
    if (locked) return
    locked = true
    lockBodyScroll()
  }

  function unlock(): void {
    if (!locked) return
    locked = false
    unlockBodyScroll()
  }

  onBeforeUnmount(unlock)
  return { lock, unlock }
}
