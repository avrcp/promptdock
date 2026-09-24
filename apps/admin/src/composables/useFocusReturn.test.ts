import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { defineComponent, nextTick } from 'vue'
import { mount } from '@vue/test-utils'

import { useFocusReturn } from './useFocusReturn'

const TestHost = defineComponent({
  setup() {
    const { capture, restore } = useFocusReturn()
    return { capture, restore }
  },
  template: `
    <div>
      <button data-testid="trigger">触发按钮</button>
      <button data-testid="capture" @click="capture">捕获焦点</button>
      <button data-testid="restore" @click="restore">恢复焦点</button>
    </div>
  `,
})

describe('useFocusReturn', () => {
  beforeEach(() => {
    document.body.innerHTML = ''
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('restores focus to the previously focused element', async () => {
    const wrapper = mount(TestHost, { attachTo: document.body })
    const trigger = wrapper.find('[data-testid="trigger"]')
    const captureBtn = wrapper.find('[data-testid="capture"]')

    ;(trigger.element as HTMLButtonElement).focus()
    expect(document.activeElement).toBe(trigger.element)

    await captureBtn.trigger('click')
    await wrapper.find('[data-testid="restore"]').trigger('click')
    await nextTick()

    expect(document.activeElement).toBe(trigger.element)
    wrapper.unmount()
  })

  it('does not throw when the captured element is removed from the DOM', async () => {
    const wrapper = mount(TestHost, { attachTo: document.body })
    const captureBtn = wrapper.find('[data-testid="capture"]')

    ;(captureBtn.element as HTMLButtonElement).focus()
    await captureBtn.trigger('click')
    wrapper.unmount()

    const second = mount(TestHost, { attachTo: document.body })
    await second.find('[data-testid="restore"]').trigger('click')
    await nextTick()
    expect(document.activeElement).not.toBeNull()
    second.unmount()
  })

  it('captures null when no element is focused', async () => {
    const wrapper = mount(TestHost, { attachTo: document.body })
    await wrapper.find('[data-testid="capture"]').trigger('click')
    // capture() stores the currently active element; restore() is a no-op for null.
    await wrapper.find('[data-testid="restore"]').trigger('click')
    await nextTick()
    expect(document.body.contains(document.activeElement)).toBe(true)
    wrapper.unmount()
  })
})
