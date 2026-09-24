import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'

import SystemPage from './SystemPage.vue'
import { setActiveScenario } from '@/composables/useScenarioSelector'
import { adminRepository } from '@/composables/useScenarioSelector'
import { AdminRepositoryError } from '@/contracts/error'
import type { ScenarioId } from '@/data/mock/scenarios'

function makeRouter(): ReturnType<typeof createRouter> {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/overview', name: 'overview', component: { template: '<div />' } },
      { path: '/devices', name: 'devices', component: { template: '<div />' } },
      { path: '/wechat', name: 'wechat', component: { template: '<div />' } },
      { path: '/queue', name: 'queue', component: { template: '<div />' } },
      { path: '/system', name: 'system', component: SystemPage },
    ],
  })
}

async function mountPage(): Promise<ReturnType<typeof mount>> {
  const router = makeRouter()
  await router.push('/system')
  await router.isReady()
  return mount(SystemPage, {
    global: { plugins: [router] },
    attachTo: document.body,
  })
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

function findButtonByText(text: string): HTMLButtonElement | null {
  const buttons = Array.from(document.body.querySelectorAll('button'))
  return buttons.find((b) => b.textContent?.trim() === text) ?? null
}

describe('SystemPage states and operations', () => {
  beforeEach(() => {
    setActiveScenario('healthy' satisfies ScenarioId)
    document.body.innerHTML = ''
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders loading skeleton on first paint', () => {
    const router = makeRouter()
    router.push('/system')
    const wrapper = mount(SystemPage, {
      global: { plugins: [router] },
      attachTo: document.body,
    })
    expect(wrapper.find('[data-testid="system-skeleton"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('renders build, database, workers and configuration sections', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.text().includes('mock-relay/0.1.0'))
    expect(wrapper.text()).toContain('构建')
    expect(wrapper.text()).toContain('数据库')
    expect(wrapper.text()).toContain('Workers')
    expect(wrapper.text()).toContain('配置摘要')
    expect(wrapper.findAll('.system__card')).toHaveLength(5)
    // 6 workers from healthy scenario
    expect(wrapper.findAll('.system__worker').length).toBe(6)
    expect(wrapper.text()).toContain('回路')
    wrapper.unmount()
  })

  it('shows degraded database integrity under database-degraded scenario', async () => {
    setActiveScenario('database-degraded')
    const wrapper = await mountPage()
    await waitFor(() => wrapper.text().includes('outbox-worker'))
    expect(wrapper.findAll('[data-testid="status-chip"]').map((c) => c.text())).toContain('降级')
    expect(wrapper.find('[data-testid="system-database-panel"]').classes()).toContain(
      'system__card--attention',
    )
    wrapper.unmount()
  })

  it('runs retention cleanup and shows a success receipt', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="system-run-retention"]').exists())
    expect(wrapper.find('[data-testid="system-run-retention"]').classes()).toContain(
      'app-button--danger',
    )
    findButtonByText('执行保留清理')?.click()
    await waitFor(
      () => document.body.querySelector('[data-testid="retention-scope-preview"]') !== null,
    )
    expect(document.body.textContent).toContain('终态入站命令')
    findButtonByText('确认并执行清理')?.click()
    await waitFor(() => wrapper.text().includes('保留清理已完成'))
    expect(wrapper.text()).toContain('mock retention pass complete')
    wrapper.unmount()
  })

  it('generates a safe diagnostics summary without secrets', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="system-generate-diagnostics"]').exists())
    findButtonByText('生成安全诊断摘要')?.click()
    await waitFor(() => wrapper.find('[data-testid="system-diagnostics-alert"]').exists())
    expect(wrapper.find('[data-testid="system-diagnostics-alert"]').text()).toContain(
      '未包含任何密钥',
    )
    expect(wrapper.find('[data-testid="system-diagnostics-alert"] .copy-value').exists()).toBe(true)
    expect(wrapper.text()).not.toMatch(/mock-pdv2\.not-a-real-secret/)
    wrapper.unmount()
  })

  it('shows section-level error when system snapshot fails', async () => {
    vi.spyOn(adminRepository.read, 'getSystemSnapshot').mockRejectedValue(
      new AdminRepositoryError({
        code: 'INTERNAL',
        message: '系统快照读取失败。',
        retryable: true,
        requestId: null,
      }),
    )
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="system-error"]').exists())
    expect(wrapper.find('[data-testid="system-error"]').text()).toContain('系统快照读取失败')
    wrapper.unmount()
  })

  it('labels the diagnostics summary as mock data in non-production builds', async () => {
    const wrapper = await mountPage()
    await waitFor(() => wrapper.find('[data-testid="system-generate-diagnostics"]').exists())
    findButtonByText('生成安全诊断摘要')?.click()
    await waitFor(() => wrapper.find('[data-testid="system-diagnostics-alert"]').exists())
    const text = wrapper.find('[data-testid="system-diagnostics-alert"]').text()
    expect(text).toContain('（模拟数据）')
    wrapper.unmount()
  })
})
