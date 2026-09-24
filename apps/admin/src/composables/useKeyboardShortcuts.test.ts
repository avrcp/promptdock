import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { defineComponent } from 'vue'
import type { Router } from 'vue-router'
import { mount } from '@vue/test-utils'
import {
  useKeyboardShortcuts,
  NAV_SHORTCUTS,
  type KeyboardShortcutsApi,
} from './useKeyboardShortcuts'

function makeRouter(initialPath = '/'): Router {
  return {
    currentRoute: { value: { path: initialPath } },
    push: vi.fn(),
  } as unknown as Router
}

function fakeEvent(overrides: Partial<KeyboardEvent> & { key: string }): KeyboardEvent {
  return {
    isComposing: false,
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    preventDefault: vi.fn(),
    ...overrides,
  } as unknown as KeyboardEvent
}

function mountHarness(router: Router): { api: KeyboardShortcutsApi } {
  const Comp = defineComponent({
    setup() {
      const api = useKeyboardShortcuts(router)
      return { api }
    },
    render() {
      return null
    },
  })
  const wrapper = mount(Comp)
  return { api: (wrapper.vm as { api: KeyboardShortcutsApi }).api }
}

describe('useKeyboardShortcuts', () => {
  it('maps every nav shortcut to its route', () => {
    expect(NAV_SHORTCUTS).toMatchObject({
      o: '/overview',
      d: '/devices',
      w: '/wechat',
      q: '/queue',
      s: '/system',
    })
  })

  it('navigates with the g-prefix sequence', () => {
    const router = makeRouter('/')
    const { api } = mountHarness(router)
    api.handleKeydown(fakeEvent({ key: 'g' }))
    api.handleKeydown(fakeEvent({ key: 'o' }))
    expect(router.push).toHaveBeenCalledWith('/overview')
  })

  it('navigates each destination', () => {
    const cases: [string, string][] = [
      ['d', '/devices'],
      ['w', '/wechat'],
      ['q', '/queue'],
      ['s', '/system'],
    ]
    for (const [key, dest] of cases) {
      const router = makeRouter('/')
      const { api } = mountHarness(router)
      api.handleKeydown(fakeEvent({ key: 'g' }))
      api.handleKeydown(fakeEvent({ key }))
      expect(router.push).toHaveBeenCalledWith(dest)
    }
  })

  it('does not navigate when already on the destination', () => {
    const router = makeRouter('/overview')
    const { api } = mountHarness(router)
    api.handleKeydown(fakeEvent({ key: 'g' }))
    api.handleKeydown(fakeEvent({ key: 'o' }))
    expect(router.push).not.toHaveBeenCalled()
  })

  it('toggles the help dialog with ?', () => {
    const router = makeRouter('/')
    const { api } = mountHarness(router)
    expect(api.showHelp.value).toBe(false)
    api.handleKeydown(fakeEvent({ key: '?' }))
    expect(api.showHelp.value).toBe(true)
    api.handleKeydown(fakeEvent({ key: '?' }))
    expect(api.showHelp.value).toBe(false)
  })

  it('while help is open, only ? closes it (other shortcuts suppressed)', () => {
    const router = makeRouter('/')
    const { api } = mountHarness(router)
    api.handleKeydown(fakeEvent({ key: '?' }))
    expect(api.showHelp.value).toBe(true)
    api.handleKeydown(fakeEvent({ key: 'g' }))
    api.handleKeydown(fakeEvent({ key: 'o' }))
    expect(router.push).not.toHaveBeenCalled()
    expect(api.showHelp.value).toBe(true)
  })

  it('ignores shortcuts typed into form fields', () => {
    const router = makeRouter('/')
    const { api } = mountHarness(router)
    const input = document.createElement('input')
    api.handleKeydown(fakeEvent({ key: 'g', target: input }))
    api.handleKeydown(fakeEvent({ key: 'o', target: input }))
    expect(router.push).not.toHaveBeenCalled()
  })

  it('ignores IME composition and modifier combos', () => {
    const router = makeRouter('/')
    const { api } = mountHarness(router)
    api.handleKeydown(fakeEvent({ key: 'g', isComposing: true }))
    api.handleKeydown(fakeEvent({ key: 'o', isComposing: true }))
    expect(router.push).not.toHaveBeenCalled()

    api.handleKeydown(fakeEvent({ key: 'g', ctrlKey: true }))
    api.handleKeydown(fakeEvent({ key: 'o', ctrlKey: true }))
    expect(router.push).not.toHaveBeenCalled()
  })

  it('does not navigate on unknown second key after g', () => {
    const router = makeRouter('/')
    const { api } = mountHarness(router)
    api.handleKeydown(fakeEvent({ key: 'g' }))
    api.handleKeydown(fakeEvent({ key: 'z' }))
    expect(router.push).not.toHaveBeenCalled()
  })

  describe('focus page search', () => {
    let input: HTMLInputElement
    beforeEach(() => {
      input = document.createElement('input')
      input.setAttribute('data-keyboard-search', '')
      document.body.appendChild(input)
    })
    afterEach(() => {
      input.remove()
    })

    it('focuses the search field on /', () => {
      const router = makeRouter('/')
      const { api } = mountHarness(router)
      api.handleKeydown(fakeEvent({ key: '/' }))
      expect(document.activeElement).toBe(input)
    })

    it('does not prevent the browser default when no search field exists', () => {
      input.remove()
      const router = makeRouter('/')
      const { api } = mountHarness(router)
      const event = fakeEvent({ key: '/' })
      api.handleKeydown(event)
      expect(event.preventDefault).not.toHaveBeenCalled()
    })

    it('does nothing harmful when no search field exists', () => {
      input.remove()
      const router = makeRouter('/')
      const { api } = mountHarness(router)
      expect(() => api.handleKeydown(fakeEvent({ key: '/' }))).not.toThrow()
    })
  })
})
