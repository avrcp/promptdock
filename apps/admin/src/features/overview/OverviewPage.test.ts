import { describe, it, expect, beforeAll, beforeEach } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'

import OverviewPage from './OverviewPage.vue'
import { setActiveScenario } from '@/composables/useScenarioSelector'
import type { ScenarioId } from '@/data/mock/scenarios'

function makeRouter(): ReturnType<typeof createRouter> {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/overview', name: 'overview', component: OverviewPage },
      { path: '/devices', name: 'devices', component: { template: '<div />' } },
      { path: '/wechat', name: 'wechat', component: { template: '<div />' } },
      { path: '/queue', name: 'queue', component: { template: '<div />' } },
      { path: '/system', name: 'system', component: { template: '<div />' } },
    ],
  })
}

async function mountPage(): Promise<ReturnType<typeof mount>> {
  const router = makeRouter()
  await router.push('/overview')
  await router.isReady()
  return mount(OverviewPage, {
    global: {
      plugins: [router],
    },
  })
}

describe('OverviewPage states', () => {
  beforeAll(() => {
    setActiveScenario('healthy' satisfies ScenarioId)
  })

  beforeEach(() => {
    document.body.innerHTML = ''
  })

  it('renders loading skeletons synchronously on first paint', async () => {
    setActiveScenario('healthy')
    const wrapper = await mountPage()
    const busy = wrapper.find('[aria-busy="true"]')
    expect(busy.exists()).toBe(true)
    await flushPromises()
    wrapper.unmount()
  })

  it('renders healthy overview with stat cards and empty issues', async () => {
    setActiveScenario('healthy')
    const wrapper = await mountPage()
    await flushPromises()

    const relayCard = wrapper.find('[data-testid="stat-relay"]')
    expect(relayCard.text()).toContain('正常')

    const wechatCard = wrapper.find('[data-testid="stat-wechat"]')
    expect(wechatCard.text()).toContain('已就绪')

    const deviceCard = wrapper.find('[data-testid="stat-devices"]')
    expect(deviceCard.text()).toContain('2 / 3')

    const queueCard = wrapper.find('[data-testid="stat-queue"]')
    expect(queueCard.text()).toContain('0 待发 · 0 失败')

    expect(wrapper.find('[data-testid="overview-metric-strip"]').exists()).toBe(true)
    expect(wrapper.findAll('.overview__panel')).toHaveLength(4)
    expect(wrapper.find('[data-testid="overview-attention"]').exists()).toBe(false)
    expect(wrapper.text()).toContain('当前没有需要处理的问题')
    expect(wrapper.find('[data-testid="overview-error"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="wechat-error"]').exists()).toBe(false)

    const gatewayItems = wrapper.findAll('.overview__list-item')
    expect(gatewayItems.length).toBe(3)
    wrapper.unmount()
  })

  it('renders empty-first-run with zeroed devices and gateway empty state', async () => {
    setActiveScenario('empty-first-run')
    const wrapper = await mountPage()
    await flushPromises()

    const deviceCard = wrapper.find('[data-testid="stat-devices"]')
    expect(deviceCard.text()).toContain('0 / 0')

    expect(wrapper.text()).toContain('当前没有 Gateway 连接')
    expect(wrapper.text()).toContain('当前没有需要处理的问题')

    const wechatCard = wrapper.find('[data-testid="stat-wechat"]')
    expect(wechatCard.text()).toContain('未连接')
    wrapper.unmount()
  })

  it('shows section-level error when relay overview fails but keeps wechat panel', async () => {
    setActiveScenario('relay-unavailable')
    const wrapper = await mountPage()
    await flushPromises()

    const errorAlert = wrapper.find('[data-testid="overview-error"]')
    expect(errorAlert.exists()).toBe(true)
    expect(errorAlert.text()).toContain('无法连接 Relay')

    const wechatCard = wrapper.find('[data-testid="stat-wechat"]')
    expect(wechatCard.text()).toContain('已就绪')
    wrapper.unmount()
  })

  it('shows wechat section error under partial-failure while overview stays readable', async () => {
    setActiveScenario('partial-failure')
    const wrapper = await mountPage()
    await flushPromises()

    const wechatError = wrapper.find('[data-testid="wechat-error"]')
    expect(wechatError.exists()).toBe(true)

    const relayCard = wrapper.find('[data-testid="stat-relay"]')
    expect(relayCard.text()).toContain('正常')
    wrapper.unmount()
  })

  it('refresh button re-fetches data without clearing previous content', async () => {
    setActiveScenario('healthy')
    const wrapper = await mountPage()
    await flushPromises()

    setActiveScenario('queue-backlog')
    const refreshButton = wrapper.find('[data-testid="overview-refresh"]')
    expect(refreshButton.exists()).toBe(true)
    await refreshButton.trigger('click')
    await flushPromises()

    const queueCard = wrapper.find('[data-testid="stat-queue"]')
    expect(queueCard.text()).toContain('5 待发')
    expect(wrapper.find('[data-testid="overview-attention"]').text()).toContain('通知队列出现积压')
    expect(wrapper.findAll('.overview__metric')).toHaveLength(5)
    wrapper.unmount()
  })
})
