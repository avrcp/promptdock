import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'

import StatusChip from './StatusChip.vue'

describe('StatusChip live semantics', () => {
  it('is not a live region by default', () => {
    const wrapper = mount(StatusChip, {
      props: { tone: 'success', label: '正常' },
    })
    expect(wrapper.get('[data-testid="status-chip"]').attributes('role')).toBeUndefined()
  })

  it('supports an explicit polite live region when needed', () => {
    const wrapper = mount(StatusChip, {
      props: { tone: 'warning', label: '需要处理', live: 'polite' },
    })
    expect(wrapper.get('[data-testid="status-chip"]').attributes('role')).toBe('status')
  })
})

describe('StatusChip long-text hardening', () => {
  const longLabel = 'A'.repeat(120)

  it('renders a 120-character label without throwing', () => {
    const wrapper = mount(StatusChip, {
      props: { tone: 'info', label: longLabel },
    })
    expect(wrapper.get('[data-testid="status-chip"]').text()).toContain(longLabel)
    wrapper.unmount()
  })

  it('exposes a title attribute for tooltip on truncation', () => {
    const wrapper = mount(StatusChip, {
      props: { tone: 'danger', label: longLabel },
    })
    expect(wrapper.get('[data-testid="status-chip"]').attributes('title')).toBe(longLabel)
    wrapper.unmount()
  })

  it('contains a dedicated label span with the status-chip__label class', () => {
    const wrapper = mount(StatusChip, {
      props: { tone: 'success', label: longLabel },
    })
    const labelSpan = wrapper.find('.status-chip__label')
    expect(labelSpan.exists()).toBe(true)
    expect(labelSpan.text()).toBe(longLabel)
    wrapper.unmount()
  })
})
