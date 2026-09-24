import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'

import InlineAlert from './InlineAlert.vue'

describe('InlineAlert live semantics', () => {
  it('keeps warning announcements polite by default', () => {
    const wrapper = mount(InlineAlert, {
      props: { tone: 'warning', title: '需要处理' },
    })
    expect(wrapper.get('[data-testid="inline-alert"]').attributes('role')).toBe('status')
  })

  it('uses an assertive alert for danger by default', () => {
    const wrapper = mount(InlineAlert, {
      props: { tone: 'danger', title: '失败' },
    })
    expect(wrapper.get('[data-testid="inline-alert"]').attributes('role')).toBe('alert')
  })

  it('allows a high-risk warning to opt into assertive announcements', () => {
    const wrapper = mount(InlineAlert, {
      props: { tone: 'warning', title: '维护操作', live: 'assertive' },
    })
    expect(wrapper.get('[data-testid="inline-alert"]').attributes('role')).toBe('alert')
  })
})

describe('InlineAlert accessible naming', () => {
  it('uses aria-labelledby pointing to the visible title element', () => {
    const wrapper = mount(InlineAlert, {
      props: { tone: 'info', title: '连接已恢复' },
    })
    const root = wrapper.get('[data-testid="inline-alert"]')
    const labelledBy = root.attributes('aria-labelledby')
    expect(labelledBy).toBeTruthy()
    const titleEl = wrapper.find(`#${labelledBy}`)
    expect(titleEl.exists()).toBe(true)
    expect(titleEl.text()).toBe('连接已恢复')
    wrapper.unmount()
  })

  it('adds aria-describedby when a description is provided', () => {
    const wrapper = mount(InlineAlert, {
      props: { tone: 'success', title: '完成', description: '保留策略已成功执行。' },
    })
    const root = wrapper.get('[data-testid="inline-alert"]')
    const describedBy = root.attributes('aria-describedby')
    expect(describedBy).toBeTruthy()
    const descEl = wrapper.find(`#${describedBy}`)
    expect(descEl.exists()).toBe(true)
    expect(descEl.text()).toBe('保留策略已成功执行。')
    wrapper.unmount()
  })

  it('omits aria-describedby when no description is given', () => {
    const wrapper = mount(InlineAlert, {
      props: { tone: 'info', title: '提示' },
    })
    const root = wrapper.get('[data-testid="inline-alert"]')
    expect(root.attributes('aria-describedby')).toBeUndefined()
    wrapper.unmount()
  })
})
