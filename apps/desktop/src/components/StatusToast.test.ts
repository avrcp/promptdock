import { mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import StatusToast from './StatusToast.vue'

afterEach(() => {
  vi.useRealTimers()
})

describe('StatusToast', () => {
  it('dismisses success feedback after the bounded timeout', async () => {
    vi.useFakeTimers()
    const wrapper = mount(StatusToast, {
      props: { tone: 'success', message: '操作已完成。' },
    })

    await vi.advanceTimersByTimeAsync(4_499)
    expect(wrapper.emitted('dismiss')).toBeUndefined()
    await vi.advanceTimersByTimeAsync(1)
    expect(wrapper.emitted('dismiss')).toHaveLength(1)
    wrapper.unmount()
  })

  it('keeps errors until the user closes them', async () => {
    vi.useFakeTimers()
    const wrapper = mount(StatusToast, {
      props: { tone: 'danger', message: '配置读取失败。' },
    })

    await vi.advanceTimersByTimeAsync(60_000)
    expect(wrapper.emitted('dismiss')).toBeUndefined()
    expect(wrapper.get('[role="alert"]').attributes('aria-atomic')).toBe('true')
    await wrapper.get('button[aria-label="关闭提示"]').trigger('click')
    expect(wrapper.emitted('dismiss')).toHaveLength(1)
    wrapper.unmount()
  })

  it('pauses a success timeout while the toast is hovered', async () => {
    vi.useFakeTimers()
    const wrapper = mount(StatusToast, {
      props: { tone: 'success', message: '已保存。' },
    })

    await vi.advanceTimersByTimeAsync(2_000)
    await wrapper.get('.status-toast').trigger('mouseenter')
    await vi.advanceTimersByTimeAsync(10_000)
    expect(wrapper.emitted('dismiss')).toBeUndefined()
    await wrapper.get('.status-toast').trigger('mouseleave')
    await vi.advanceTimersByTimeAsync(2_500)
    expect(wrapper.emitted('dismiss')).toHaveLength(1)
    wrapper.unmount()
  })
})
