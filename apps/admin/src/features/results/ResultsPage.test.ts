import { computed, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'

const capability = vi.hoisted(() => ({ enabled: true }))
const asyncState = vi.hoisted(() => ({ data: null as unknown, refresh: vi.fn() }))
vi.mock('@/composables/useAdminCapabilities', () => ({
  useAdminCapabilities: () => ({
    can: () => capability.enabled,
    capabilityReason: () => '当前账号未授予“结果页管理”能力。',
    loading: computed(() => false),
    failed: computed(() => false),
    isReadOnly: computed(() => !capability.enabled),
  }),
}))
vi.mock('@/composables/useAsyncResource', () => ({
  useAsyncResource: () => ({
    data: ref(asyncState.data),
    loading: ref(false),
    refreshing: ref(false),
    error: ref(null),
    refresh: asyncState.refresh,
  }),
}))

import ResultsPage from './ResultsPage.vue'
import { AdminRepositoryKey } from '@/composables/useAdminRepository'
import { createAdminRepository } from '@/data/create-admin-repository'

const item = {
  resultRowId: '00000000-0000-4000-8000-000000000101',
  resultId: 'result-ui-1',
  ownerDeviceId: 'device-owner',
  safeTitle: 'Codex 最终回答',
  source: 'codex_stop',
  contentMode: 'full_final',
  sourceHash: 'a'.repeat(64),
  acceptedAt: 1,
  updatedAt: 1,
  pageState: 'available' as const,
  pageExpiresAt: 4_102_444_800_000,
  bodyBytes: 12,
  bodyPurgedAt: null,
  notificationId: 'notice-1',
  notificationStatus: 'pending_channel' as const,
  notificationAttemptCount: 0,
  notificationLastErrorCode: null,
  targetAccountFingerprint: 'wx:123456789abc',
}

function page() {
  const repository = createAdminRepository({ testMode: true })
  repository.read.listResults = vi
    .fn()
    .mockResolvedValue({ items: [item], nextCursor: null, total: 1, generatedAt: 1 })
  repository.command.revokeResult = vi.fn().mockResolvedValue({ schemaVersion: 1, ...item })
  asyncState.data = { items: [item], nextCursor: null, total: 1, generatedAt: 1 }
  return {
    repository,
    wrapper: mount(ResultsPage, {
      global: { provide: { [AdminRepositoryKey as symbol]: repository } },
    }),
  }
}

describe('ResultsPage', () => {
  afterEach(() => {
    document.body.replaceChildren()
  })
  it('loads a safe row and invokes revoke with its global result row id', async () => {
    capability.enabled = true
    const { repository, wrapper } = page()
    await flushPromises()
    await wrapper.get('button.app-button--danger').trigger('click')
    const primary = document.querySelector<HTMLButtonElement>(
      '[data-testid="app-dialog"] .app-dialog__footer button:last-child',
    )
    expect(primary).not.toBeNull()
    primary?.click()
    await flushPromises()
    expect(repository.command.revokeResult).toHaveBeenCalledWith(
      expect.objectContaining({ resultRowId: '00000000-0000-4000-8000-000000000101' }),
    )
  })

  it('reuses the same request id when a revoke attempt fails and is retried', async () => {
    capability.enabled = true
    const { repository, wrapper } = page()
    const revoke = repository.command.revokeResult as ReturnType<typeof vi.fn>
    revoke
      .mockRejectedValueOnce(new Error('temporary'))
      .mockResolvedValueOnce({ schemaVersion: 1, ...item })
    await flushPromises()
    await wrapper.get('button.app-button--danger').trigger('click')
    const primary = () =>
      document.querySelector<HTMLButtonElement>(
        '[data-testid="app-dialog"] .app-dialog__footer button:last-child',
      )
    primary()?.click()
    await flushPromises()
    primary()?.click()
    await flushPromises()
    const first = revoke.mock.calls[0]?.[0] as { requestId: string }
    const second = revoke.mock.calls[1]?.[0] as { requestId: string }
    expect(first.requestId).toBe(second.requestId)
    wrapper.unmount()
  })

  it('disables revoke without the results-manage capability', async () => {
    capability.enabled = false
    const { wrapper } = page()
    await flushPromises()
    expect(wrapper.get('button.app-button--danger').attributes('disabled')).toBeDefined()
  })

  it('renders the injected safe result row', async () => {
    capability.enabled = true
    const { wrapper } = page()
    await flushPromises()
    expect(wrapper.html()).toContain('Codex 最终回答')
  })
})
