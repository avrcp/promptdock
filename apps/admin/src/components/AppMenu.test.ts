import { beforeEach, describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { nextTick } from 'vue'

import AppMenu from './AppMenu.vue'

const items = [
  { id: 'details', label: '查看详情' },
  { id: 'disable', label: '停用设备', disabled: true, disabledReason: '设备已停用' },
  { id: 'revoke', label: '撤销设备', tone: 'danger' as const, separatorBefore: true },
]

describe('AppMenu', () => {
  beforeEach(() => {
    document.body.innerHTML = ''
  })

  it('uses button/menu/menuitem semantics and supports roving arrow, Home, and End keys', async () => {
    const wrapper = mount(AppMenu, { props: { items }, attachTo: document.body })
    const trigger = wrapper.get('[data-testid="app-menu-trigger"]')
    await trigger.trigger('keydown', { key: 'ArrowDown' })
    await nextTick()

    const popup = document.body.querySelector('[role="menu"]') as HTMLElement
    const menuItems = Array.from(popup.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'))
    expect(trigger.attributes('aria-expanded')).toBe('true')
    expect(document.activeElement).toBe(menuItems[0])

    menuItems[0]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'End', bubbles: true }))
    expect(document.activeElement).toBe(menuItems[2])
    menuItems[2]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Home', bubbles: true }))
    expect(document.activeElement).toBe(menuItems[0])
    menuItems[0]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true }))
    expect(document.activeElement).toBe(menuItems[2])
    wrapper.unmount()
  })

  it('keeps unavailable actions keyboard discoverable without allowing activation', async () => {
    const wrapper = mount(AppMenu, { props: { items }, attachTo: document.body })
    await wrapper.get('[data-testid="app-menu-trigger"]').trigger('keydown', { key: 'ArrowDown' })
    await nextTick()

    const menuItems = Array.from(
      document.body.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'),
    )
    expect(menuItems).toHaveLength(3)
    expect(menuItems[1]?.getAttribute('aria-disabled')).toBe('true')
    expect(menuItems[1]?.disabled).toBe(false)
    expect(menuItems[1]?.textContent).toContain('设备已停用')
    const reasonId = menuItems[1]?.getAttribute('aria-describedby')
    expect(reasonId).toBeTruthy()
    expect(document.getElementById(reasonId ?? '')?.textContent).toContain('设备已停用')

    menuItems[0]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }))
    expect(document.activeElement).toBe(menuItems[1])
    menuItems[1]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }))
    expect(document.activeElement).toBe(menuItems[2])

    menuItems[1]?.click()
    expect(wrapper.emitted('select')).toBeUndefined()
    expect(document.body.querySelector('[role="menu"]')).not.toBeNull()
    wrapper.unmount()
  })

  it('closes on Escape, outside pointer interaction, and selection while restoring trigger focus', async () => {
    const wrapper = mount(AppMenu, { props: { items }, attachTo: document.body })
    const trigger = wrapper.get('[data-testid="app-menu-trigger"]')
    await trigger.trigger('keydown', { key: 'ArrowDown' })
    await nextTick()
    let popup = document.body.querySelector('[role="menu"]') as HTMLElement
    popup.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await nextTick()
    expect(document.body.querySelector('[role="menu"]')).toBeNull()
    expect(document.activeElement).toBe(trigger.element)

    await trigger.trigger('click')
    await nextTick()
    document.body.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true }))
    await nextTick()
    expect(document.body.querySelector('[role="menu"]')).toBeNull()

    await trigger.trigger('click')
    await nextTick()
    popup = document.body.querySelector('[role="menu"]') as HTMLElement
    ;(popup.querySelector('[role="menuitem"]') as HTMLButtonElement).click()
    await nextTick()
    expect(wrapper.emitted('select')?.[0]?.[0]).toMatchObject({ id: 'details' })
    expect(document.activeElement).toBe(trigger.element)
    wrapper.unmount()
  })
})
