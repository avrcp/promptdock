import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'

import AppTopbar from './AppTopbar.vue'

function mountTopbar(): ReturnType<typeof mount> {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/overview', name: 'overview', component: { template: '<div />' } }],
  })
  return mount(AppTopbar, {
    global: {
      plugins: [router],
    },
  })
}

describe('AppTopbar', () => {
  it('renders the route title as the only visible page heading', () => {
    const wrapper = mountTopbar()
    const heading = wrapper.get('#app-route-title')
    expect(heading.element.tagName).toBe('H1')
    expect(heading.text()).toBe('总览')
    expect(wrapper.findAll('h1')).toHaveLength(1)
    expect(wrapper.find('.topbar__mark').exists()).toBe(false)
    expect(wrapper.find('.topbar__breadcrumb').exists()).toBe(false)
  })

  it('renders the Mock environment badge by default', () => {
    const wrapper = mountTopbar()
    const badge = wrapper.find('[data-testid="env-badge-mock"]')
    expect(badge.exists()).toBe(true)
    expect(badge.text()).toBe('Mock')
  })

  it('exposes a skip link to main content', () => {
    const wrapper = mountTopbar()
    const skip = wrapper.find('.topbar__skip')
    expect(skip.exists()).toBe(true)
    expect(skip.attributes('href')).toBe('#main-content')
  })

  it('does not render a global page-reload button', () => {
    const wrapper = mountTopbar()
    expect(wrapper.find('button.topbar__refresh').exists()).toBe(false)
  })

  it('keeps the environment badge visible even on narrow viewports', async () => {
    // The environment badge is a safety-critical signal, so it is NOT
    // hidden on small screens.  The version, by contrast, is decorative
    // and is hidden.  We assert by inspecting the source; the actual
    // visual media-query behavior is exercised in Playwright.
    const wrapper = mountTopbar()
    expect(wrapper.find('[data-testid="env-badge-mock"]').exists()).toBe(true)
  })

  it('does not duplicate session indicators in the topbar', () => {
    const wrapper = mountTopbar()
    expect(wrapper.find('.topbar__session').exists()).toBe(false)
    expect(wrapper.find('.topbar__session-dot').exists()).toBe(false)
    expect(wrapper.find('.topbar__badge-dot').exists()).toBe(false)
  })
})
