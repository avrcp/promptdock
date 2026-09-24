import { describe, it, expect, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { nextTick } from 'vue'

import AppDialog from './AppDialog.vue'

async function mountDialog(props: Record<string, unknown> = {}): Promise<ReturnType<typeof mount>> {
  return mount(AppDialog, {
    props: {
      open: true,
      title: '测试对话框',
      ...props,
    },
    slots: {
      default: '<button type="button" data-testid="slot-button">插槽按钮</button>',
    },
    attachTo: document.body,
  })
}

function keydown(target: Element, key: string, shiftKey = false): void {
  const event = new KeyboardEvent('keydown', { key, shiftKey, bubbles: true, cancelable: true })
  target.dispatchEvent(event)
}

describe('AppDialog accessibility', () => {
  beforeEach(() => {
    document.body.innerHTML = ''
  })

  it('uses a labelled modal dialog and links its visible description when provided', async () => {
    const wrapper = await mountDialog({ description: '这是一段说明文字' })
    const dialog = document.body.querySelector('[role="dialog"]')
    const title = dialog?.querySelector('.app-dialog__title')
    const description = dialog?.querySelector('.app-dialog__desc')
    expect(dialog?.getAttribute('aria-modal')).toBe('true')
    expect(dialog?.getAttribute('aria-labelledby')).toBe(title?.id)
    expect(dialog?.getAttribute('aria-describedby')).toBe(description?.id)
    wrapper.unmount()
  })

  it('moves initial focus to the cancel (secondary) button by default', async () => {
    const wrapper = await mountDialog()
    await new Promise((r) => setTimeout(r, 0))
    const active = document.activeElement
    expect(active?.textContent?.trim()).toContain('取消')
    wrapper.unmount()
  })

  it('moves initial focus to the primary button when initialFocus=primary', async () => {
    const wrapper = await mountDialog({ initialFocus: 'primary' })
    await new Promise((r) => setTimeout(r, 0))
    const active = document.activeElement
    expect(active?.textContent?.trim()).toContain('确认')
    wrapper.unmount()
  })

  it('uses the primary action as the default focus when no secondary action exists', async () => {
    const wrapper = await mountDialog({ showSecondary: false })
    await new Promise((r) => setTimeout(r, 0))
    expect(document.activeElement?.textContent?.trim()).toContain('确认')
    wrapper.unmount()
  })

  it('falls back to an available control when its preferred action is disabled', async () => {
    const wrapper = await mountDialog({
      initialFocus: 'primary',
      primaryDisabled: true,
      showSecondary: false,
      closable: false,
    })
    await new Promise((r) => setTimeout(r, 0))
    const dialog = document.body.querySelector('[role="dialog"]') as HTMLElement
    expect(dialog.contains(document.activeElement)).toBe(true)
    expect(document.activeElement?.getAttribute('data-testid')).toBe('slot-button')
    wrapper.unmount()
  })

  it('traps Tab focus inside the dialog', async () => {
    const wrapper = await mountDialog()
    await new Promise((r) => setTimeout(r, 0))
    const dialog = document.body.querySelector('[role="dialog"]') as HTMLElement
    const focusables = Array.from(
      dialog.querySelectorAll<HTMLElement>(
        'button:not([disabled]),input:not([disabled]),[tabindex]:not([tabindex="-1"])',
      ),
    )
    const first = focusables[0]
    const last = focusables[focusables.length - 1]
    expect(first).toBeDefined()
    expect(last).toBeDefined()

    // Tab from the last element wraps to the first.
    last!.focus()
    keydown(last!, 'Tab')
    expect(document.activeElement).toBe(first)

    // Shift+Tab from the first element wraps to the last.
    first!.focus()
    keydown(first!, 'Tab', true)
    expect(document.activeElement).toBe(last)
    wrapper.unmount()
  })

  it('closes on Escape and restores focus to the previously focused element', async () => {
    const trigger = document.createElement('button')
    trigger.textContent = '打开'
    document.body.appendChild(trigger)
    trigger.focus()

    const wrapper = await mountDialog()
    await new Promise((r) => setTimeout(r, 0))

    const dialog = document.body.querySelector('[role="dialog"]') as HTMLElement
    keydown(dialog, 'Escape')
    expect(wrapper.emitted('update:open')?.[0]?.[0]).toBe(false)

    // Parent responds to update:open by closing the dialog.
    await wrapper.setProps({ open: false })
    await new Promise((r) => setTimeout(r, 0))

    expect(document.activeElement).toBe(trigger)
    wrapper.unmount()
  })

  it('allows a workflow to keep the dialog open after the secondary action', async () => {
    const wrapper = await mountDialog({ closeOnSecondary: false })
    const buttons = document.body.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')
    buttons[2]?.click()
    expect(wrapper.emitted('secondary')).toHaveLength(1)
    expect(wrapper.emitted('update:open')).toBeUndefined()
    wrapper.unmount()
  })

  it('supports a single-action footer when secondary action would be redundant', async () => {
    const wrapper = mount(AppDialog, {
      props: { open: true, title: '扫码登录', primaryLabel: '取消登录', showSecondary: false },
      attachTo: document.body,
    })
    await nextTick()
    const buttons = document.body.querySelectorAll('[role="dialog"] [data-testid="app-button"]')
    expect(buttons).toHaveLength(1)
    expect(buttons[0]?.textContent).toContain('取消登录')
    wrapper.unmount()
  })

  it('locks close, Escape, backdrop, and secondary dismissal while pending', async () => {
    const wrapper = await mountDialog({ pending: true })
    const dialog = document.body.querySelector('[role="dialog"]') as HTMLElement
    keydown(dialog, 'Escape')
    ;(document.body.querySelector('.app-dialog__backdrop') as HTMLElement).click()
    const buttons = document.body.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')
    const secondary = Array.from(buttons).find((button) => button.textContent?.includes('取消'))
    expect(buttons[0]?.disabled).toBe(true)
    expect(secondary?.disabled).toBe(true)
    expect(wrapper.emitted('update:open')).toBeUndefined()
    expect(wrapper.emitted('secondary')).toBeUndefined()
    wrapper.unmount()
  })
})
