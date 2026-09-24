import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { Circle } from 'lucide-vue-next'

import AppButton from './AppButton.vue'
import AppIconButton from './AppIconButton.vue'
import CopyValue from './CopyValue.vue'
import EmptyState from './EmptyState.vue'
import SkeletonBlock from './SkeletonBlock.vue'
import StatusChip from './StatusChip.vue'

const originalClipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, 'clipboard')

afterEach(() => {
  vi.useRealTimers()
  vi.restoreAllMocks()
  if (originalClipboardDescriptor) {
    Object.defineProperty(navigator, 'clipboard', originalClipboardDescriptor)
  } else {
    Reflect.deleteProperty(navigator, 'clipboard')
  }
})

describe('shared controls', () => {
  it('renders button variants and retains disabled busy semantics while loading', () => {
    const wrapper = mount(AppButton, {
      props: { variant: 'primary', loading: true, loadingLabel: '正在保存' },
      slots: { default: '保存' },
    })

    const button = wrapper.get('[data-testid="app-button"]')
    expect(button.classes()).toContain('app-button--primary')
    expect(button.attributes('disabled')).toBeDefined()
    expect(button.attributes('aria-busy')).toBe('true')
    expect(button.text()).toContain('正在保存')
    expect(wrapper.find('.app-button__spinner').exists()).toBe(true)
  })

  it('requires a specific accessible label and preserves icon-button tone', () => {
    const wrapper = mount(AppIconButton, {
      props: { icon: Circle, tone: 'danger', ariaLabel: '撤销设备' },
    })
    const button = wrapper.get('[data-testid="app-icon-button"]')

    expect(button.attributes('aria-label')).toBe('撤销设备')
    expect(button.classes()).toContain('app-icon-button--danger')
  })

  it('renders an actionable empty state without dropping its status semantics', () => {
    const wrapper = mount(EmptyState, {
      props: { icon: Circle, title: '没有设备', description: '请先创建设备。' },
      slots: { action: '<button type="button">新建设备</button>' },
    })

    expect(wrapper.get('[data-testid="empty-state"]').attributes('role')).toBe('status')
    expect(wrapper.text()).toContain('新建设备')
    expect(wrapper.find('.empty-state__icon').exists()).toBe(true)
  })

  it('renders skeletons as decorative by default and supports announce mode', () => {
    const wrapper = mount(SkeletonBlock)
    const skeleton = wrapper.get('[data-testid="skeleton"]')

    expect(skeleton.attributes('aria-hidden')).toBe('true')
    expect(skeleton.attributes('role')).toBeUndefined()

    const announcing = mount(SkeletonBlock, { props: { announce: true } })
    const liveSkeleton = announcing.get('[data-testid="skeleton"]')
    expect(liveSkeleton.attributes('role')).toBe('status')
    expect(liveSkeleton.attributes('aria-busy')).toBe('true')
  })

  it('uses a dot with explicit text for status meaning', () => {
    const wrapper = mount(StatusChip, { props: { tone: 'warning', label: '数据可能已过期' } })

    expect(wrapper.find('.status-chip__dot').exists()).toBe(true)
    expect(wrapper.text()).toContain('数据可能已过期')
    expect(wrapper.get('[data-testid="status-chip"]').attributes('aria-label')).toBe(
      '状态：数据可能已过期',
    )
  })

  it('shows copy feedback for two seconds after a successful copy', async () => {
    vi.useFakeTimers()
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
    const wrapper = mount(CopyValue, { props: { value: 'pdv_test_value' } })

    await wrapper.get('[data-testid="app-icon-button"]').trigger('click')
    await Promise.resolve()
    expect(writeText).toHaveBeenCalledWith('pdv_test_value')
    expect(wrapper.text()).toContain('已复制')

    await vi.advanceTimersByTimeAsync(1999)
    expect(wrapper.text()).toContain('已复制')
    await vi.advanceTimersByTimeAsync(1)
    expect(wrapper.text()).not.toContain('已复制')
  })

  it('shows visible failure feedback when clipboard write is rejected', async () => {
    vi.useFakeTimers()
    const writeText = vi.fn().mockRejectedValue(new Error('denied'))
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
    const wrapper = mount(CopyValue, { props: { value: 'secret' } })

    await wrapper.get('[data-testid="app-icon-button"]').trigger('click')
    // Allow the rejected promise to settle
    await vi.advanceTimersByTimeAsync(0)

    expect(wrapper.find('.copy-value__error').exists()).toBe(true)
    expect(wrapper.find('.copy-value__error').text()).toBe('复制失败')
    wrapper.unmount()
  })

  it('cleans up previous timer on repeated copy clicks', async () => {
    vi.useFakeTimers()
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
    const wrapper = mount(CopyValue, { props: { value: 'val' } })
    const btn = wrapper.get('[data-testid="app-icon-button"]')

    await btn.trigger('click')
    await Promise.resolve()
    expect(wrapper.text()).toContain('已复制')

    // Click again before timeout — should reset without leaking
    await btn.trigger('click')
    await Promise.resolve()
    expect(wrapper.text()).toContain('已复制')

    // Only one timer's worth of advancement needed
    await vi.advanceTimersByTimeAsync(2000)
    expect(wrapper.text()).not.toContain('已复制')
    wrapper.unmount()
  })
})
