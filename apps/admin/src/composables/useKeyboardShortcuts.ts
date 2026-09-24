import { onBeforeUnmount, onMounted, ref, type Ref } from 'vue'
import type { Router } from 'vue-router'

/**
 * Global keyboard shortcuts for the Relay admin console (manual §18).
 *
 *   ?        toggle this help
 *   /        focus the current page's search (only pages that expose one)
 *   g o      go to Overview
 *   g d      go to Devices
 *   g w      go to WeChat
 *   g q      go to Queue
 *   g s      go to System
 *
 * All shortcuts are suppressed while the user is typing, composing with an
 * IME, or holding a modifier key, so they never collide with form fields,
 * screen-reader shortcuts, or the browser's own keys.
 */

export const NAV_SHORTCUTS: Readonly<Record<string, string>> = {
  o: '/overview',
  d: '/devices',
  w: '/wechat',
  q: '/queue',
  s: '/system',
}

export const SHORTCUT_HELP: { keys: string; label: string }[] = [
  { keys: '?', label: '打开 / 关闭快捷键帮助' },
  { keys: '/', label: '聚焦当前页搜索（仅支持搜索的页面）' },
  { keys: 'g o', label: '总览' },
  { keys: 'g d', label: '设备' },
  { keys: 'g w', label: '微信通道' },
  { keys: 'g q', label: '队列' },
  { keys: 'g s', label: '系统' },
]

const PREFIX_TIMEOUT_MS = 800

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  const tag = target.tagName
  if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return true
  if (target.isContentEditable) return true
  return false
}

function focusPageSearch(): boolean {
  const el = document.querySelector<HTMLElement>('[data-keyboard-search]')
  if (!el) return false
  el.focus()
  el.scrollIntoView?.({ block: 'nearest' })
  return true
}

export interface KeyboardShortcutsApi {
  showHelp: Ref<boolean>
  openHelp: () => void
  closeHelp: () => void
  handleKeydown: (event: KeyboardEvent) => void
}

export function useKeyboardShortcuts(router: Router): KeyboardShortcutsApi {
  const showHelp = ref(false)
  let prefix: 'g' | null = null
  let prefixTimer: ReturnType<typeof setTimeout> | null = null

  function clearPrefix(): void {
    prefix = null
    if (prefixTimer !== null) {
      clearTimeout(prefixTimer)
      prefixTimer = null
    }
  }

  function handleKeydown(event: KeyboardEvent): void {
    // IME composition must never be hijacked by a shortcut.
    if (event.isComposing) return
    if (isEditableTarget(event.target)) return

    if (event.metaKey || event.ctrlKey || event.altKey) return

    // While the help dialog is open, only `?` toggles it closed.
    if (showHelp.value) {
      if (event.key === '?') {
        event.preventDefault()
        showHelp.value = false
      }
      return
    }

    if (event.key === '?') {
      event.preventDefault()
      showHelp.value = true
      return
    }

    if (prefix === 'g') {
      const dest = NAV_SHORTCUTS[event.key.toLowerCase()]
      clearPrefix()
      if (dest) {
        event.preventDefault()
        if (router.currentRoute.value.path !== dest) {
          void router.push(dest)
        }
      }
      return
    }

    if (event.key === 'g') {
      prefix = 'g'
      if (prefixTimer !== null) clearTimeout(prefixTimer)
      prefixTimer = setTimeout(clearPrefix, PREFIX_TIMEOUT_MS)
      return
    }

    if (event.key === '/') {
      if (focusPageSearch()) event.preventDefault()
    }
  }

  onMounted(() => {
    window.addEventListener('keydown', handleKeydown)
  })
  onBeforeUnmount(() => {
    window.removeEventListener('keydown', handleKeydown)
    clearPrefix()
  })

  return {
    showHelp,
    openHelp: () => {
      showHelp.value = true
    },
    closeHelp: () => {
      showHelp.value = false
    },
    handleKeydown,
  }
}
