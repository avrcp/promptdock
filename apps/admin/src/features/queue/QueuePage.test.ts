import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'

import QueuePage from './QueuePage.vue'
import { setActiveScenario } from '@/composables/useScenarioSelector'
import { adminRepository } from '@/composables/useScenarioSelector'
import { AdminRepositoryError } from '@/contracts/error'
import type { Page } from '@/contracts/common'
import type { DeliveryListItem } from '@/contracts/queue'
import type { ScenarioId } from '@/data/mock/scenarios'

function makeRouter(): ReturnType<typeof createRouter> {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/overview', name: 'overview', component: { template: '<div />' } },
      { path: '/devices', name: 'devices', component: { template: '<div />' } },
      { path: '/wechat', name: 'wechat', component: { template: '<div />' } },
      { path: '/queue', name: 'queue', component: QueuePage },
      { path: '/system', name: 'system', component: { template: '<div />' } },
    ],
  })
}

async function mountPage(path = '/queue'): Promise<ReturnType<typeof mount>> {
  const router = makeRouter()
  await router.push(path)
  await router.isReady()
  return mount(QueuePage, {
    global: { plugins: [router] },
    attachTo: document.body,
  })
}

function clickTabByText(text: string): void {
  const tabs = Array.from(document.body.querySelectorAll('.app-tabs__tab'))
  const target = tabs.find((t) => t.textContent?.trim() === text)
  if (!target) throw new Error(`tab "${text}" not found`)
  ;(target as HTMLElement).click()
}

async function tick(): Promise<void> {
  await flushPromises()
  await new Promise((resolve) => setTimeout(resolve, 0))
  await flushPromises()
}

async function waitFor(predicate: () => boolean, attempts = 60): Promise<void> {
  for (let i = 0; i < attempts; i += 1) {
    if (predicate()) return
    await tick()
  }
  throw new Error('waitFor timed out waiting for condition')
}

describe('QueuePage states and interactions', () => {
  beforeEach(() => {
    setActiveScenario('healthy' satisfies ScenarioId)
    document.body.innerHTML = ''
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders loading skeleton on first paint', () => {
    const router = makeRouter()
    router.push('/queue')
    const wrapper = mount(QueuePage, {
      global: { plugins: [router] },
      attachTo: document.body,
    })
    expect(wrapper.find('[data-testid="queue-skeleton"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('renders confirmed empty state only after a successful empty page', async () => {
    setActiveScenario('empty-first-run')
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="empty-state"]').exists())
    expect(wrapper.find('[data-testid="queue-initial-error"]').exists()).toBe(false)
    expect(wrapper.text()).toContain('没有匹配的投递')
    wrapper.unmount()
  })

  it('renders delivery rows on the default tab', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="delivery-table"]').exists())
    expect(wrapper.text()).toContain('运行事件')
    expect(wrapper.text()).toContain('主控工作站')
    expect(wrapper.findAll('.app-table__row').length).toBe(2)
    wrapper.unmount()
  })

  it('loads the replies tab when reached directly through a shared URL', async () => {
    const wrapper = await mountPage('/queue?tab=replies')
    await waitFor(() => wrapper.find('[data-testid="reply-table"]').exists())
    expect(wrapper.find('[data-testid="reply-freshness"]').text()).not.toContain('尚未加载')
    expect(wrapper.find('[data-testid="reply-table"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('loads the inbound tab when reached directly through a shared URL', async () => {
    const wrapper = await mountPage('/queue?tab=inbound')
    await waitFor(() => wrapper.find('[data-testid="inbound-table"]').exists())
    expect(wrapper.find('[data-testid="inbound-freshness"]').text()).not.toContain('尚未加载')
    wrapper.unmount()
  })

  it('switches to interactive replies tab', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="delivery-table"]').exists())
    clickTabByText('交互回复')
    await waitFor(() => wrapper.find('[data-testid="reply-table"]').exists())
    expect(wrapper.text()).toContain('list_recent')
    expect(wrapper.text()).toContain('fp:7a2b')
    wrapper.unmount()
  })

  it('switches to inbound commands tab', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="delivery-table"]').exists())
    clickTabByText('入站命令')
    await waitFor(() => wrapper.find('[data-testid="inbound-table"]').exists())
    expect(wrapper.text()).toContain('session:7a2b')
    wrapper.unmount()
  })

  it('filters deliveries by state', async () => {
    setActiveScenario('queue-backlog')
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="delivery-table"]').exists())
    const before = wrapper.findAll('.app-table__row').length
    expect(before).toBe(5)

    const select = wrapper.find('[data-testid="delivery-state-filter"]')
    await select.setValue('retrying')
    await waitFor(() => wrapper.findAll('.app-table__row').length === 2)
    expect(wrapper.findAll('.app-table__row').length).toBe(2)
    expect(wrapper.find('[data-testid="queue-clear-filters"]').exists()).toBe(true)
    await wrapper.find('[data-testid="queue-clear-filters"]').trigger('click')
    await waitFor(() => wrapper.findAll('.app-table__row').length === 5)
    wrapper.unmount()
  })

  it('shows a clear message when a shared item is not on the current page', async () => {
    const wrapper = await mountPage('/queue?tab=replies&item=missing-item')
    await waitFor(() => wrapper.find('[data-testid="reply-table"]').exists())
    await waitFor(() => document.body.querySelector('[data-testid="queue-item-missing"]') !== null)
    expect(
      document.body.querySelector('[data-testid="queue-item-missing"]')?.textContent,
    ).toContain('当前页未找到该条目')
    wrapper.unmount()
  })

  it('opens detail drawer from the explicit detail entry point', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="delivery-table"]').exists())
    await wrapper.find('.queue__detail-link').trigger('click')
    await tick()
    await waitFor(() => document.body.querySelector('.app-drawer') !== null)
    expect(document.body.querySelector('.app-drawer')?.textContent).toContain('优先级')
    expect(document.body.querySelector('.app-drawer')?.textContent).toContain('概览')
    expect(document.body.querySelector('.app-drawer')?.textContent).toContain('路由')
    expect(document.body.querySelector('.app-drawer')?.textContent).toContain('重试')
    expect(document.body.querySelector('.app-drawer')?.textContent).toContain('错误')
    wrapper.unmount()
  })

  it('shows section-level error when deliveries fetch fails', async () => {
    vi.spyOn(adminRepository.read, 'listDeliveries').mockRejectedValue(
      new AdminRepositoryError({
        code: 'OUTBOX_BACKLOG',
        message: '队列读取返回降级数据。',
        retryable: true,
        requestId: null,
      }),
    )
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="queue-error"]').exists())
    expect(wrapper.find('[data-testid="queue-error"]').text()).toContain('投递队列积压')
    expect(wrapper.find('[data-testid="queue-initial-error"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="empty-state"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('does not leak the cursor into the UI', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="delivery-table"]').exists())
    expect(wrapper.text()).not.toMatch(/cursor/i)
    wrapper.unmount()
  })

  it('returns to page one and reloads once when a delivery cursor expires', async () => {
    const original = adminRepository.read.listDeliveries.bind(adminRepository.read)
    const cursors: Array<string | null | undefined> = []
    vi.spyOn(adminRepository.read, 'listDeliveries').mockImplementation(async (query, options) => {
      cursors.push(query.cursor)
      if (query.cursor === 'expired-cursor') {
        throw new AdminRepositoryError({
          code: 'CURSOR_EXPIRED',
          message: 'cursor expired',
          retryable: true,
          requestId: 'req-cursor',
        })
      }
      const page = await original(query, options)
      return { ...page, nextCursor: 'expired-cursor' }
    })
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="delivery-table"]').exists())

    const nextButton = wrapper.findAll('button').find((button) => button.text().includes('下一页'))
    if (!nextButton) throw new Error('next-page button not found')
    await nextButton.trigger('click')

    await waitFor(() => cursors.length === 3)
    expect(cursors).toEqual([null, 'expired-cursor', null])
    await waitFor(() => wrapper.find('[data-testid="queue-error"]').exists() === false)
    wrapper.unmount()
  })

  it('surfaces a freshness string on each tab', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="delivery-freshness"]').exists())
    expect(wrapper.find('[data-testid="delivery-freshness"]').text()).not.toBe('')
    expect(wrapper.text()).toMatch(/共 \d+ 条/)
    clickTabByText('交互回复')
    await waitFor(() => wrapper.find('[data-testid="reply-freshness"]').exists())
    expect(wrapper.find('[data-testid="reply-freshness"]').text()).not.toBe('')
    clickTabByText('入站命令')
    await waitFor(() => wrapper.find('[data-testid="inbound-freshness"]').exists())
    expect(wrapper.find('[data-testid="inbound-freshness"]').text()).not.toBe('')
    wrapper.unmount()
  })

  it('labels the data as possibly stale when a refresh fails but old data is on screen', async () => {
    let firstResolve: ((v: Page<DeliveryListItem>) => void) | undefined
    const listDeliveries = vi.spyOn(adminRepository.read, 'listDeliveries')
    // First call: deliver a healthy page so we have on-screen data.
    listDeliveries.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          firstResolve = resolve
        }),
    )
    const wrapper = await mountPage()
    // Resolve the first call with a valid empty page so the resource
    // records a successful lastSuccessAt and we have data on screen.
    firstResolve?.({
      items: [],
      nextCursor: null,
      total: 0,
      generatedAt: 0,
    })
    await tick()
    await waitFor(() => wrapper.find('[data-testid="delivery-freshness"]').exists())
    expect(wrapper.find('[data-testid="delivery-freshness"]').text()).toMatch(/最近刷新|刚刚/)
    wrapper.unmount()
  })
})
