import { beforeEach, describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'

import AppDrawer from './AppDrawer.vue'

function keydown(target: Element, key: string, shiftKey = false): void {
  target.dispatchEvent(
    new KeyboardEvent('keydown', { key, shiftKey, bubbles: true, cancelable: true }),
  )
}

async function mountDrawer(): Promise<ReturnType<typeof mount>> {
  return mount(AppDrawer, {
    props: { open: true, title: '设备详情' },
    slots: { default: '<button type="button" data-testid="drawer-action">执行操作</button>' },
    attachTo: document.body,
  })
}

describe('AppDrawer accessibility', () => {
  beforeEach(() => {
    document.body.innerHTML = ''
  })

  it('uses a labelled modal dialog and focuses its close button on opening', async () => {
    const wrapper = await mountDrawer()
    await new Promise((resolve) => setTimeout(resolve, 0))
    const drawer = document.body.querySelector('[role="dialog"]')
    const title = drawer?.querySelector('.app-drawer__title')
    expect(drawer?.getAttribute('aria-modal')).toBe('true')
    expect(drawer?.getAttribute('aria-labelledby')).toBe(title?.id)
    expect((document.activeElement as HTMLElement | null)?.getAttribute('aria-label')).toBe(
      '关闭抽屉',
    )
    wrapper.unmount()
  })

  it('traps focus and restores the trigger after Escape', async () => {
    const trigger = document.createElement('button')
    trigger.textContent = '查看设备'
    document.body.appendChild(trigger)
    trigger.focus()

    const wrapper = await mountDrawer()
    await new Promise((resolve) => setTimeout(resolve, 0))
    const drawer = document.body.querySelector('[role="dialog"]') as HTMLElement
    const close = drawer.querySelector('button') as HTMLButtonElement
    const action = drawer.querySelector('[data-testid="drawer-action"]') as HTMLButtonElement

    action.focus()
    keydown(action, 'Tab')
    expect(document.activeElement).toBe(close)

    keydown(drawer, 'Escape')
    expect(wrapper.emitted('update:open')?.[0]?.[0]).toBe(false)
    await wrapper.setProps({ open: false })
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(document.activeElement).toBe(trigger)
    wrapper.unmount()
  })

  it('keeps initial focus in a pending or non-closable drawer', async () => {
    const wrapper = mount(AppDrawer, {
      props: { open: true, title: '设备详情', pending: true, closable: false },
      attachTo: document.body,
    })
    await new Promise((resolve) => setTimeout(resolve, 0))
    const drawer = document.body.querySelector('[role="dialog"]') as HTMLElement
    expect(drawer.contains(document.activeElement)).toBe(true)
    expect(document.activeElement).toBe(drawer)
    wrapper.unmount()
  })

  it('does not dismiss a pending drawer through Escape, backdrop, or close button', async () => {
    const wrapper = mount(AppDrawer, {
      props: { open: true, title: '设备详情', pending: true },
      attachTo: document.body,
    })
    await new Promise((resolve) => setTimeout(resolve, 0))
    const drawer = document.body.querySelector('[role="dialog"]') as HTMLElement
    keydown(drawer, 'Escape')
    ;(document.body.querySelector('.app-drawer__backdrop') as HTMLElement).click()
    const close = drawer.querySelector('button') as HTMLButtonElement
    expect(close.disabled).toBe(true)
    expect(wrapper.emitted('update:open')).toBeUndefined()
    wrapper.unmount()
  })

  it('renders a long title with the truncation-ready title class', async () => {
    const longTitle = '设备详情 — ' + 'X'.repeat(200)
    const wrapper = mount(AppDrawer, {
      props: { open: true, title: longTitle },
      slots: { default: '<p>内容</p>' },
      attachTo: document.body,
    })
    await new Promise((resolve) => setTimeout(resolve, 0))
    const titleEl = document.body.querySelector('.app-drawer__title')
    expect(titleEl).toBeTruthy()
    expect(titleEl?.textContent).toBe(longTitle)
    wrapper.unmount()
  })
})
