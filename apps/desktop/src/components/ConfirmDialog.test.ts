import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import ConfirmDialog from './ConfirmDialog.vue'

describe('ConfirmDialog', () => {
  beforeEach(() => {
    document.body.innerHTML = '<main id="app"><button id="trigger">撤销链接</button></main>'
    Object.defineProperty(HTMLDialogElement.prototype, 'showModal', {
      configurable: true,
      value() {},
    })
    Object.defineProperty(HTMLDialogElement.prototype, 'close', {
      configurable: true,
      value() {},
    })
    vi.spyOn(HTMLDialogElement.prototype, 'showModal').mockImplementation(function (
      this: HTMLDialogElement,
    ) {
      this.setAttribute('open', '')
    })
    vi.spyOn(HTMLDialogElement.prototype, 'close').mockImplementation(function (
      this: HTMLDialogElement,
    ) {
      this.removeAttribute('open')
    })
  })

  afterEach(() => {
    vi.restoreAllMocks()
    document.body.innerHTML = ''
  })

  it('初始即打开时进入原生模态、聚焦取消并使应用背景 inert', async () => {
    document.querySelector<HTMLButtonElement>('#trigger')!.focus()
    const wrapper = mount(ConfirmDialog, {
      attachTo: document.body,
      props: {
        open: true,
        title: '撤销结果链接？',
        description: '撤销后此链接不再可读。',
        confirmLabel: '撤销链接',
      },
    })

    await flushPromises()
    const dialog = document.querySelector<HTMLDialogElement>('dialog')!
    expect(HTMLDialogElement.prototype.showModal).toHaveBeenCalledOnce()
    expect(dialog.open).toBe(true)
    expect(document.querySelector('#app')?.hasAttribute('inert')).toBe(true)
    expect(document.activeElement?.textContent).toContain('取消')
    wrapper.unmount()
  })

  it('Escape 取消，关闭后恢复触发元素焦点与背景原状态', async () => {
    const trigger = document.querySelector<HTMLButtonElement>('#trigger')!
    trigger.focus()
    const wrapper = mount(ConfirmDialog, {
      attachTo: document.body,
      props: {
        open: true,
        title: '撤销结果链接？',
        description: '撤销后此链接不再可读。',
        confirmLabel: '撤销链接',
      },
    })
    await flushPromises()
    document.querySelector<HTMLDialogElement>('dialog')!.dispatchEvent(new Event('cancel'))
    await flushPromises()
    expect(wrapper.emitted('cancel')).toHaveLength(1)

    await wrapper.setProps({ open: false })
    await flushPromises()
    expect(document.querySelector('#app')?.hasAttribute('inert')).toBe(false)
    expect(document.activeElement).toBe(trigger)
    wrapper.unmount()
  })

  it('快速关闭与卸载会清理异步焦点、原生 open 和 inert', async () => {
    const wrapper = mount(ConfirmDialog, {
      attachTo: document.body,
      props: {
        open: false,
        title: '撤销结果链接？',
        description: '撤销后此链接不再可读。',
        confirmLabel: '撤销链接',
      },
    })
    await wrapper.setProps({ open: true })
    await wrapper.setProps({ open: false })
    await flushPromises()
    expect(document.querySelector('#app')?.hasAttribute('inert')).toBe(false)

    await wrapper.setProps({ open: true })
    await flushPromises()
    expect(document.querySelector('#app')?.hasAttribute('inert')).toBe(true)
    wrapper.unmount()
    expect(document.querySelector('#app')?.hasAttribute('inert')).toBe(false)
  })
})
