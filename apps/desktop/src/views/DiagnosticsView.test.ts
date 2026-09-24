import { flushPromises, mount } from '@vue/test-utils'
import { defineComponent, h } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { provideDesktopState } from '../desktop/desktopState'
import DiagnosticsView from './DiagnosticsView.vue'
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
const Host = defineComponent({
  setup() {
    provideDesktopState()
    return () => h(DiagnosticsView)
  },
})
describe('DiagnosticsView', () => {
  it('does not start health or notification probes on mount', () => {
    mount(Host)
    expect(
      vi.mocked(invoke).mock.calls.some(([name]) => String(name).startsWith('desktop_health_')),
    ).toBe(false)
  })
  it('rejects malformed health steps instead of rendering unknown actions', async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      steps: [
        {
          id: 'x',
          label: 'unsafe',
          status: 'invented',
          code: null,
          detail: 'x',
          nextAction: 'delete',
        },
      ],
      report: {},
      test: null,
    } as never)
    const wrapper = mount(Host)
    await wrapper
      .findAll('button')
      .find((button) => button.text().includes('运行本机体检'))!
      .trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('体检回执无法确认')
    expect(wrapper.text()).not.toContain('unsafe')
  })
  it('checks an uncertain test before retrying and keeps the same request identity', async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'desktop_health_test_start') throw new Error('timeout')
      if (command === 'desktop_health_test_status')
        return {
          probeId: 'probe-safe',
          outboxId: 'outbox-safe',
          status: 'submitted',
          remoteStatus: null,
          lastErrorCode: null,
        } as never
      return null as never
    })
    const wrapper = mount(Host)
    await wrapper
      .findAll('button')
      .find((button) => button.text().includes('发送测试通知'))!
      .trigger('click')
    await flushPromises()
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('desktop_health_test_status')
    const starts = vi
      .mocked(invoke)
      .mock.calls.filter(([command]) => command === 'desktop_health_test_start')
    expect(starts).toHaveLength(1)
    expect(wrapper.text()).toContain('probe-sa')
  })
})
