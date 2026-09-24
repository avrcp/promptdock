export const FOCUSABLE_SELECTOR = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

export function isProgrammaticallyAvailable(element: HTMLElement | null): element is HTMLElement {
  if (!element) return false
  if (element.hidden || element.getAttribute('aria-disabled') === 'true') return false
  if (element.closest('[aria-hidden="true"], [inert]')) return false
  if ('disabled' in element && Boolean((element as HTMLButtonElement).disabled)) return false
  return true
}

export function focusPreferredOrFirst(
  root: HTMLElement | null,
  preferred: HTMLElement | null,
): void {
  if (!root) return
  if (isProgrammaticallyAvailable(preferred) && root.contains(preferred)) {
    preferred.focus()
    return
  }

  const first = Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).find(
    isProgrammaticallyAvailable,
  )
  ;(first ?? root).focus()
}
