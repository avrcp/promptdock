import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'

import AppTabs from './AppTabs.vue'

const tabs = [
  { id: 'deliveries', label: '通知投递' },
  { id: 'replies', label: '交互回复' },
  { id: 'inbound', label: '入站命令' },
]

function mountTabs(
  modelValue = 'deliveries',
  propsOverride: Record<string, unknown> = {},
): ReturnType<typeof mount> {
  return mount(AppTabs, {
    props: { tabs, modelValue, ...propsOverride },
    attachTo: document.body,
  })
}

describe('AppTabs accessibility', () => {
  it('renders a tablist with tab roles and roving tabindex', () => {
    const wrapper = mountTabs()
    expect(wrapper.find('[role="tablist"]').attributes('aria-label')).toBe('页面标签')
    const tabEls = wrapper.findAll('[role="tab"]')
    expect(tabEls.length).toBe(3)
    expect(tabEls[0]!.attributes('aria-selected')).toBe('true')
    expect(tabEls[0]!.attributes('tabindex')).toBe('0')
    expect(tabEls[1]!.attributes('tabindex')).toBe('-1')
    wrapper.unmount()
  })

  it('links tabs to a base id, deriving per-tab panel ids', () => {
    // Each tab gets its own panel id so screen readers can map
    // aria-controls → aria-labelledby 1:1, not all three tabs sharing one
    // panel.
    const wrapper = mountTabs('deliveries', { baseId: 'queue' })
    const tabEls = wrapper.findAll('[role="tab"]')
    expect(tabEls[0]?.attributes('id')).toBe('queue-tab-deliveries')
    expect(tabEls[0]?.attributes('aria-controls')).toBe('queue-panel-deliveries')
    expect(tabEls[1]?.attributes('id')).toBe('queue-tab-replies')
    expect(tabEls[1]?.attributes('aria-controls')).toBe('queue-panel-replies')
    expect(tabEls[2]?.attributes('id')).toBe('queue-tab-inbound')
    expect(tabEls[2]?.attributes('aria-controls')).toBe('queue-panel-inbound')
    wrapper.unmount()
  })

  it('falls back to a runtime id when no baseId is supplied', () => {
    const wrapper = mountTabs('deliveries')
    const first = wrapper.findAll('[role="tab"]')[0]
    // A non-empty id is still produced so aria-controls / aria-labelledby
    // can resolve.
    expect(first?.attributes('id')).toBeTruthy()
    expect(first?.attributes('id')).toMatch(/-tab-deliveries$/)
    expect(first?.attributes('aria-controls')).toMatch(/-panel-deliveries$/)
    wrapper.unmount()
  })

  it('moves to the next tab on ArrowRight with wrap-around', async () => {
    const wrapper = mountTabs()
    const first = wrapper.findAll('[role="tab"]')[0]
    await first!.trigger('keydown', { key: 'ArrowRight' })
    expect(wrapper.emitted('update:modelValue')?.[0]?.[0]).toBe('replies')
    expect(document.activeElement).toBe(wrapper.findAll('[role="tab"]')[1]?.element)

    // Wrap-around: from the last tab, ArrowRight returns to the first.
    await wrapper.setProps({ modelValue: 'inbound' })
    const last = wrapper.findAll('[role="tab"]')[2]
    await last!.trigger('keydown', { key: 'ArrowRight' })
    expect(wrapper.emitted('update:modelValue')?.[1]?.[0]).toBe('deliveries')
    wrapper.unmount()
  })

  it('moves to the previous tab on ArrowLeft with wrap-around', async () => {
    const wrapper = mountTabs('inbound')
    const last = wrapper.findAll('[role="tab"]')[2]
    await last!.trigger('keydown', { key: 'ArrowLeft' })
    expect(wrapper.emitted('update:modelValue')?.[0]?.[0]).toBe('replies')

    // Wrap-around: from the first tab, ArrowLeft returns to the last.
    await wrapper.setProps({ modelValue: 'deliveries' })
    const first = wrapper.findAll('[role="tab"]')[0]
    await first!.trigger('keydown', { key: 'ArrowLeft' })
    expect(wrapper.emitted('update:modelValue')?.[1]?.[0]).toBe('inbound')
    wrapper.unmount()
  })

  it('jumps to first tab on Home and last tab on End', async () => {
    const wrapper = mountTabs('replies')
    const middle = wrapper.findAll('[role="tab"]')[1]
    await middle!.trigger('keydown', { key: 'Home' })
    expect(wrapper.emitted('update:modelValue')?.[0]?.[0]).toBe('deliveries')
    await middle!.trigger('keydown', { key: 'End' })
    expect(wrapper.emitted('update:modelValue')?.[1]?.[0]).toBe('inbound')
    wrapper.unmount()
  })

  it('ignores unrelated keys', async () => {
    const wrapper = mountTabs()
    const first = wrapper.findAll('[role="tab"]')[0]
    await first!.trigger('keydown', { key: 'Enter' })
    expect(wrapper.emitted('update:modelValue')).toBeUndefined()
    wrapper.unmount()
  })
})
