import { describe, it, expect, beforeEach } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'

import WeChatPage from './WechatPage.vue'
import { setActiveScenario } from '@/composables/useScenarioSelector'
import type { ScenarioId } from '@/data/mock/scenarios'

function makeRouter(): ReturnType<typeof createRouter> {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/overview', name: 'overview', component: { template: '<div />' } },
      { path: '/devices', name: 'devices', component: { template: '<div />' } },
      { path: '/wechat', name: 'wechat', component: WeChatPage },
      { path: '/queue', name: 'queue', component: { template: '<div />' } },
      { path: '/system', name: 'system', component: { template: '<div />' } },
    ],
  })
}

async function mountPage(): Promise<ReturnType<typeof mount>> {
  const router = makeRouter()
  await router.push('/wechat')
  await router.isReady()
  return mount(WeChatPage, {
    global: { plugins: [router] },
    attachTo: document.body,
  })
}

function findButtonByText(text: string): HTMLButtonElement | null {
  const buttons = Array.from(document.body.querySelectorAll('button'))
  return buttons.find((b) => b.textContent?.trim() === text) ?? null
}

function findDialogButtonByText(text: string): HTMLButtonElement | null {
  const dialog = document.body.querySelector('.app-dialog')
  if (!dialog) return null
  const buttons = Array.from(dialog.querySelectorAll('button'))
  return buttons.find((b) => b.textContent?.trim() === text) ?? null
}

function clickByTestId(testid: string): void {
  const el = document.querySelector(`[data-testid="${testid}"]`)
  if (!el) throw new Error(`button [data-testid="${testid}"] not found`)
  ;(el as HTMLElement).click()
}

function mainChips(wrapper: ReturnType<typeof mount>): string[] {
  return wrapper.findAll('[data-testid="status-chip"]').map((c) => c.text())
}

function dialogChips(): string[] {
  const dialog = document.body.querySelector('.app-dialog')
  if (!dialog) return []
  return Array.from(dialog.querySelectorAll('[data-testid="status-chip"]')).map(
    (c) => c.textContent?.trim() ?? '',
  )
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

describe('WeChatPage states and interactions', () => {
  beforeEach(() => {
    setActiveScenario('healthy' satisfies ScenarioId)
    document.body.innerHTML = ''
  })

  it('renders loading skeleton on first paint', () => {
    const router = makeRouter()
    router.push('/wechat')
    const wrapper = mount(WeChatPage, {
      global: { plugins: [router] },
      attachTo: document.body,
    })
    expect(wrapper.find('[data-testid="wechat-skeleton"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('renders ready status, metrics and channel events', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="wechat-login"]').exists())
    expect(mainChips(wrapper)).toContain('已就绪')
    expect(wrapper.text()).toContain('运行指标')
    expect(wrapper.text()).toContain('最近通道事件')
    expect(wrapper.text()).toContain('微信通道轮询正常')
    wrapper.unmount()
  })

  it('maps disconnected scenario to 未连接', async () => {
    setActiveScenario('wechat-disconnected')
    const wrapper = await mountPage()
    await waitFor(() => mainChips(wrapper).includes('未连接'))
    wrapper.unmount()
  })

  it('maps disconnected login-authorization scenario to 未连接', async () => {
    setActiveScenario('wechat-login')
    const wrapper = await mountPage()
    await waitFor(() => mainChips(wrapper).includes('未连接'))
    wrapper.unmount()
  })

  it('opens a real-shape login snapshot without synthetic state controls', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="wechat-login"]').exists())

    clickByTestId('wechat-login')
    await tick()
    await waitFor(() => document.body.querySelector('[data-testid="wechat-qr"]') !== null)

    expect(dialogChips()).toContain('请扫码')
    expect(findButtonByText('模拟手机扫码')).toBeNull()
    expect(findButtonByText('模拟确认')).toBeNull()
    wrapper.unmount()
  })

  it('cancelling closes the dialog and clears the in-memory QR snapshot', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="wechat-login"]').exists())
    clickByTestId('wechat-login')
    await waitFor(() => document.body.querySelector('[data-testid="wechat-qr"]') !== null)

    findButtonByText('取消登录')?.click()
    await waitFor(() => document.body.querySelector('[data-testid="wechat-qr"]') === null)
    expect(document.body.querySelector('.app-dialog')).toBeNull()
    wrapper.unmount()
  })

  it('sends a test message and shows acceptance text', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="wechat-test"]').exists())
    clickByTestId('wechat-test')
    await waitFor(() => wrapper.text().includes('Relay 已接管'))
    wrapper.unmount()
  })

  it('disconnects after confirm dialog', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="wechat-disconnect"]').exists())
    clickByTestId('wechat-disconnect')
    await waitFor(
      () =>
        document.body.querySelector('.app-dialog')?.textContent?.includes('断开微信通道') ?? false,
    )
    expect(document.body.textContent).toContain('只会断开 Relay')
    expect(document.body.textContent).toContain('不会恢复 Desktop Local')
    expect(document.body.textContent).toContain('重新登录')
    findDialogButtonByText('断开连接')?.click()
    await waitFor(
      () =>
        document.body.querySelector('.app-dialog') === null ||
        !document.body.querySelector('.app-dialog')?.textContent?.includes('断开微信通道'),
    )
    wrapper.unmount()
  })
})
