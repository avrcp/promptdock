import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'

import AppSidebar from './AppSidebar.vue'

function mountSidebar(): ReturnType<typeof mount> {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/overview', name: 'overview', component: { template: '<div />' } },
      { path: '/devices', name: 'devices', component: { template: '<div />' } },
      { path: '/wechat', name: 'wechat', component: { template: '<div />' } },
      { path: '/queue', name: 'queue', component: { template: '<div />' } },
      { path: '/results', name: 'results', component: { template: '<div />' } },
      { path: '/system', name: 'system', component: { template: '<div />' } },
    ],
  })
  return mount(AppSidebar, {
    global: {
      plugins: [router],
    },
  })
}

describe('AppSidebar', () => {
  it('renders the navigation links with Chinese labels', () => {
    const wrapper = mountSidebar()
    const links = wrapper.findAll('a.sidebar__link')
    expect(links).toHaveLength(6)
    const labels = links.map((link) => link.text().trim())
    expect(labels).toEqual(['总览', '设备', '微信通道', '队列', '结果页', '系统'])
  })

  it('marks links with the correct hrefs', () => {
    const wrapper = mountSidebar()
    const links = wrapper.findAll('a.sidebar__link')
    const hrefs = links.map((link) => link.attributes('href'))
    expect(hrefs).toEqual(['/overview', '/devices', '/wechat', '/queue', '/results', '/system'])
  })

  it('exposes a landmark nav element', () => {
    const wrapper = mountSidebar()
    const nav = wrapper.find('nav.sidebar')
    expect(nav.exists()).toBe(true)
    expect(nav.attributes('aria-label')).toBe('主导航')
  })

  it('keeps every icon-only tablet destination explicitly named', () => {
    const wrapper = mountSidebar()
    expect(wrapper.findAll('a.sidebar__link').map((link) => link.attributes('aria-label'))).toEqual(
      ['总览', '设备', '微信通道', '队列', '结果页', '系统'],
    )
  })
})
