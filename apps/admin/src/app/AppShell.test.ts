import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { createMemoryHistory, createRouter } from 'vue-router'

import AppShell from './AppShell.vue'

afterEach(() => {
  vi.restoreAllMocks()
  Reflect.deleteProperty(HTMLElement.prototype, 'scrollTo')
})

describe('AppShell scroll ownership', () => {
  it('resets the internal workbench on path navigation but preserves position for query state', async () => {
    const scrollTo = vi.fn()
    Object.defineProperty(HTMLElement.prototype, 'scrollTo', {
      configurable: true,
      value: scrollTo,
    })
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [
        { path: '/overview', component: { template: '<div>总览</div>' } },
        { path: '/devices', component: { template: '<div>设备</div>' } },
      ],
    })
    await router.push('/overview')
    await router.isReady()

    const wrapper = mount(AppShell, {
      global: {
        plugins: [router],
        stubs: {
          AppTopbar: true,
          AppSidebar: true,
          DevScenarioSelector: true,
          SessionStatusBar: true,
          KeyboardShortcuts: true,
        },
      },
    })

    await router.push('/overview?panel=attention')
    await flushPromises()
    expect(scrollTo).not.toHaveBeenCalled()

    await router.push('/devices')
    await flushPromises()
    expect(scrollTo).toHaveBeenCalledOnce()
    expect(scrollTo).toHaveBeenCalledWith({ top: 0, left: 0, behavior: 'auto' })
    wrapper.unmount()
  })
})
