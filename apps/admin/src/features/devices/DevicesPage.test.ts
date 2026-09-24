import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'

import DevicesPage from './DevicesPage.vue'
import { adminRepository, setActiveScenario } from '@/composables/useScenarioSelector'
import { AdminRepositoryError } from '@/contracts/error'
import type { ScenarioId } from '@/data/mock/scenarios'

function makeRouter(): ReturnType<typeof createRouter> {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/overview', name: 'overview', component: { template: '<div />' } },
      { path: '/devices', name: 'devices', component: DevicesPage },
      { path: '/wechat', name: 'wechat', component: { template: '<div />' } },
      { path: '/queue', name: 'queue', component: { template: '<div />' } },
      { path: '/system', name: 'system', component: { template: '<div />' } },
    ],
  })
}

async function mountPage(path = '/devices'): Promise<ReturnType<typeof mount>> {
  const router = makeRouter()
  await router.push(path)
  await router.isReady()
  return mount(DevicesPage, {
    global: {
      plugins: [router],
    },
    attachTo: document.body,
  })
}

function findDialogButton(text: string): HTMLButtonElement | null {
  const buttons = Array.from(document.body.querySelectorAll('button'))
  return buttons.find((b) => b.textContent?.trim() === text) ?? null
}

async function tick(): Promise<void> {
  await flushPromises()
  await new Promise((resolve) => setTimeout(resolve, 0))
  await flushPromises()
}

async function waitFor(predicate: () => boolean, attempts = 20): Promise<void> {
  for (let i = 0; i < attempts; i += 1) {
    if (predicate()) return
    await tick()
  }
  throw new Error('waitFor timed out waiting for condition')
}

async function openDeviceMenu(
  wrapper: ReturnType<typeof mount>,
  index: number,
): Promise<HTMLButtonElement[]> {
  const triggers = wrapper.findAll('[data-testid="app-menu-trigger"]')
  await triggers[index]?.trigger('click')
  await waitFor(() => document.body.querySelector('[role="menu"]') !== null)
  return Array.from(document.body.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'))
}

async function selectDeviceAction(
  wrapper: ReturnType<typeof mount>,
  index: number,
  label: string,
): Promise<void> {
  const item = (await openDeviceMenu(wrapper, index)).find((button) =>
    button.textContent?.includes(label),
  )
  expect(item, `missing device menu action: ${label}`).not.toBeNull()
  item?.click()
  await tick()
}

describe('DevicesPage states and interactions', () => {
  beforeEach(() => {
    setActiveScenario('healthy' satisfies ScenarioId)
    document.body.innerHTML = ''
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders loading skeletons synchronously on first paint', async () => {
    const wrapper = await mountPage()
    expect(wrapper.find('[data-testid="devices-skeleton"]').exists()).toBe(true)
    await flushPromises()
    wrapper.unmount()
  })

  it('renders healthy device list with three fixture devices', async () => {
    const wrapper = await mountPage()
    await tick()

    const rows = wrapper.findAll('.app-table__row')
    expect(rows.length).toBe(3)
    expect(wrapper.text()).toContain('主控工作站')
    expect(wrapper.text()).toContain('移动协作机')
    expect(wrapper.text()).toContain('备用采集节点')

    const chips = wrapper.findAll('[data-testid="status-chip"]')
    const chipTexts = chips.map((c) => c.text())
    expect(chipTexts).toContain('在线')
    expect(chipTexts).toContain('离线')
    wrapper.unmount()
  })

  it('does not claim the device list is empty when the first read fails', async () => {
    vi.spyOn(adminRepository.read, 'listDevices').mockRejectedValue(
      new AdminRepositoryError({
        code: 'RELAY_UNAVAILABLE',
        message: 'Relay 暂不可用',
        retryable: true,
        requestId: 'req-devices',
      }),
    )
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="devices-error"]').exists())
    expect(wrapper.find('[data-testid="devices-initial-error"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="empty-state"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('renders empty state for empty-first-run scenario', async () => {
    setActiveScenario('empty-first-run')
    const wrapper = await mountPage()
    await tick()

    expect(wrapper.find('[data-testid="empty-state"]').exists()).toBe(true)
    expect(wrapper.text()).toContain('没有匹配的设备')
    wrapper.unmount()
  })

  it('renders devices normally under partial-failure scenario', async () => {
    setActiveScenario('partial-failure')
    const wrapper = await mountPage()
    await tick()

    const rows = wrapper.findAll('.app-table__row')
    expect(rows.length).toBe(3)
    expect(wrapper.text()).toContain('主控工作站')
    wrapper.unmount()
  })

  it('filters by state through the URL query', async () => {
    const wrapper = await mountPage()
    await tick()

    await wrapper.find('[data-state="online"]').trigger('click')
    await waitFor(() => wrapper.findAll('.app-table__row').length === 2)

    const rows = wrapper.findAll('.app-table__row')
    expect(rows.length).toBe(2)
    wrapper.unmount()
  })

  it('treats an obsolete unknown URL filter as no filter', async () => {
    const wrapper = await mountPage('/devices?state=unknown')
    await tick()

    expect(wrapper.findAll('.app-table__row')).toHaveLength(3)
    wrapper.unmount()
  })

  it('searches by device name', async () => {
    const wrapper = await mountPage()
    await tick()

    const input = wrapper.find('[data-testid="device-search"]')
    ;(input.element as HTMLInputElement).value = '主控'
    await input.trigger('input')
    await new Promise((r) => setTimeout(r, 250))
    await waitFor(() => wrapper.findAll('.app-table__row').length === 1)

    const rows = wrapper.findAll('.app-table__row')
    expect(rows.length).toBe(1)
    expect(rows[0]?.text()).toContain('主控工作站')
    wrapper.unmount()
  })

  it('shows the matching count and clears filters from the device toolbar', async () => {
    const wrapper = await mountPage()
    await tick()

    await wrapper.find('[data-state="online"]').trigger('click')
    await waitFor(() => wrapper.text().includes('2 台设备'))
    await wrapper.get('.device-filters__clear').trigger('click')
    await waitFor(() => wrapper.findAll('.app-table__row').length === 3)
    wrapper.unmount()
  })

  it('opens the detail drawer from the dedicated detail action', async () => {
    const wrapper = await mountPage()
    await tick()

    await wrapper.findAll('[data-testid="device-details"]')[0]?.trigger('click')
    await waitFor(
      () =>
        document.body
          .querySelector('[data-testid="app-drawer"] aside')
          ?.textContent?.includes('主控工作站') ?? false,
    )

    const drawer = document.body.querySelector('[data-testid="app-drawer"] aside')
    expect(drawer).not.toBeNull()
    expect(drawer?.textContent).toContain('主控工作站')
    expect(drawer?.textContent).toContain('mock-uuid-a1b2c3d4')

    const escapeEvent = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })
    drawer?.dispatchEvent(escapeEvent)
    await waitFor(() => document.body.querySelector('[data-testid="app-drawer"]') === null)
    wrapper.unmount()
  })

  it('creates a device via AdminCommandRepository and shows one-time synthetic receipt', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.findAll('.app-table__row').length === 3)
    const before = wrapper.findAll('.app-table__row').length

    await wrapper.find('[data-testid="device-create"]').trigger('click')
    await waitFor(() => document.body.querySelector('.app-dialog') !== null)

    const nameInput = document.body.querySelector(
      '[data-testid="device-create-name"]',
    ) as HTMLInputElement | null
    const scopeInput = document.body.querySelector(
      '[data-testid="device-create-scopes"]',
    ) as HTMLFieldSetElement | null
    expect(nameInput).not.toBeNull()
    expect(scopeInput?.querySelectorAll('input[type="checkbox"]')).toHaveLength(7)
    expect(document.body.querySelector('[data-testid="device-create-client"]')).toBeNull()
    expect(
      (scopeInput?.querySelector('input[data-scope="gateway:connect"]') as HTMLInputElement)
        .checked,
    ).toBe(true)
    nameInput!.value = '测试新设备'
    nameInput!.dispatchEvent(new Event('input', { bubbles: true }))
    await tick()

    const confirmBtn = findDialogButton('创建并显示 Token')
    expect(confirmBtn).not.toBeNull()
    confirmBtn!.click()
    await waitFor(() => document.body.querySelector('[data-testid="credential-receipt"]') !== null)

    const tokenEl = document.body.querySelector(
      '[data-testid="credential-receipt-token"]',
    ) as HTMLTextAreaElement | null
    expect(tokenEl).not.toBeNull()
    expect(tokenEl!.value.startsWith('mock-pdv2.not-a-real-secret.')).toBe(true)
    expect(tokenEl!.value.startsWith('pdv2.')).toBe(false)

    await waitFor(() => wrapper.findAll('.app-table__row').length === before + 1)
    expect(wrapper.findAll('.app-table__row').length).toBe(before + 1)

    const closeBtn = findDialogButton('我已保存，关闭')
    closeBtn?.click()
    await waitFor(() => document.body.querySelector('[data-testid="credential-receipt"]') === null)
    expect(document.body.querySelector('[data-testid="credential-receipt"]')).toBeNull()
    wrapper.unmount()
  })

  it('rotates a device credential via confirm dialog and shows receipt', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.findAll('.app-table__row').length === 3)

    await selectDeviceAction(wrapper, 0, 'Rotate Token')
    await waitFor(
      () =>
        document.body.querySelector('.app-dialog')?.textContent?.includes('Rotate Token') ?? false,
    )

    const primaryBtn = findDialogButton('Rotate Token')
    primaryBtn?.click()
    await waitFor(() => document.body.querySelector('[data-testid="credential-receipt"]') !== null)

    const tokenEl = document.body.querySelector(
      '[data-testid="credential-receipt-token"]',
    ) as HTMLTextAreaElement | null
    expect(tokenEl).not.toBeNull()
    expect(tokenEl!.value.startsWith('mock-pdv2.not-a-real-secret.')).toBe(true)
    expect(tokenEl!.value.startsWith('pdv2.')).toBe(false)
    wrapper.unmount()
  })

  it('disables then enables a device through command repository', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.findAll('.app-table__row').length === 3)

    await selectDeviceAction(wrapper, 0, '禁用设备')
    await waitFor(() => findDialogButton('禁用') !== null)
    const confirmDisable = findDialogButton('禁用')
    confirmDisable?.click()
    await waitFor(() =>
      wrapper
        .findAll('[data-testid="status-chip"]')
        .map((c) => c.text())
        .includes('已禁用'),
    )

    await selectDeviceAction(wrapper, 0, '启用设备')
    await waitFor(() => findDialogButton('启用') !== null)
    const confirmEnable = findDialogButton('启用')
    confirmEnable?.click()
    await waitFor(
      () =>
        !wrapper
          .findAll('[data-testid="status-chip"]')
          .map((c) => c.text())
          .includes('已禁用'),
    )
    wrapper.unmount()
  })

  it('revokes a device and marks further actions unavailable', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.findAll('.app-table__row').length === 3)

    await selectDeviceAction(wrapper, 2, '撤销设备')
    await waitFor(
      () => document.body.querySelector('.app-dialog')?.textContent?.includes('撤销') ?? false,
    )

    const confirmRevoke = findDialogButton('撤销')
    confirmRevoke?.click()
    await waitFor(() =>
      wrapper
        .findAll('[data-testid="status-chip"]')
        .map((c) => c.text())
        .includes('已撤销'),
    )

    await wrapper.find('[data-state="revoked"]').trigger('click')
    await waitFor(() => wrapper.findAll('.app-table__row').length === 1)
    const revokedRows = wrapper.findAll('.app-table__row')
    expect(revokedRows.length).toBe(1)

    const disabledAction = (await openDeviceMenu(wrapper, 0)).find((button) =>
      button.textContent?.includes('Rotate Token'),
    )
    expect(disabledAction?.getAttribute('aria-disabled')).toBe('true')
    expect(disabledAction?.disabled).toBe(false)
    expect(disabledAction?.textContent).toContain('当前状态不支持')
    wrapper.unmount()
  })
})
