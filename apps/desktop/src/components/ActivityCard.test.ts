import { flushPromises, mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import ActivityCard from './ActivityCard.vue'
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const item = {
  runKey: 'run-safe',
  workspaceLabel: 'workspace',
  displayTitle: '安全标题',
  activityRevision: 4,
  phase: 'settling' as const,
  startedAt: null,
  lastObservedAt: 1_788_970_000,
  attention: {
    revision: 4,
    acknowledgedRevision: 3,
    label: '等待处理',
    historical: false,
    observationExpiresAt: null,
  },
  result: null,
  delivery: null,
}

describe('ActivityCard', () => {
  it('shows result-open failure and only marks the displayed revision seen after success', async () => {
    const shown = {
      ...item,
      result: {
        outboxId: 'result-2',
        resultRevision: 2,
        pageState: 'available',
        expiresAt: Date.now() + 60000,
        seenResultRevision: 0,
      },
    }
    const wrapper = mount(ActivityCard, { props: { item: shown } })
    vi.mocked(invoke).mockRejectedValueOnce(new Error('offline'))
    const button = wrapper.findAll('button').find((button) => button.text() === '打开结果')!
    await button.trigger('click')
    await flushPromises()
    expect(wrapper.get('[role="alert"]').text()).toContain('未能打开')
    expect(wrapper.emitted('markSeen')).toBeUndefined()
    vi.mocked(invoke).mockResolvedValueOnce(undefined)
    await button.trigger('click')
    await flushPromises()
    expect(wrapper.emitted('markSeen')?.[0]).toEqual([shown])
  })
  it('keeps task text out while exposing metadata actions by keyboard-native buttons', async () => {
    const wrapper = mount(ActivityCard, { props: { item } })
    expect(wrapper.text()).toContain('安全标题')
    expect(wrapper.findAll('button')).toHaveLength(3)
    await wrapper.get('button').trigger('click')
    expect(wrapper.emitted('select')?.[0]).toEqual(['run-safe'])
    await wrapper
      .findAll('button')
      .find((button) => button.text() === '确认已知晓')!
      .trigger('click')
    expect(wrapper.emitted('acknowledge')?.[0]).toEqual([item])
  })
})
