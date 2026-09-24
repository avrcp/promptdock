import { beforeEach, describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'

import CredentialReceiptDialog from './CredentialReceiptDialog.vue'

const receipt = {
  receiptId: 'receipt-01',
  action: 'create' as const,
  deviceId: 'device-01',
  oneTimeToken: 'mock-pdv2.not-a-real-secret.one-time-token',
  issuedAt: 1_769_000_000_000,
}

describe('CredentialReceiptDialog', () => {
  beforeEach(() => {
    document.body.innerHTML = ''
  })

  it('keeps the one-time credential in the rendered read-only field without persisting it', () => {
    const wrapper = mount(CredentialReceiptDialog, {
      props: {
        open: true,
        receipt,
        deviceName: '主控',
        actionLabel: '新建设备',
        actionHint: '立即保存',
      },
      attachTo: document.body,
    })
    const token = document.body.querySelector('[data-testid="credential-receipt-token"]')
    expect(token?.getAttribute('readonly')).not.toBeNull()
    expect((token as HTMLTextAreaElement).value).toBe(receipt.oneTimeToken)
    wrapper.unmount()
  })

  it('locks every dismissal control while the credential workflow is pending', async () => {
    const wrapper = mount(CredentialReceiptDialog, {
      props: {
        open: true,
        receipt,
        deviceName: '主控',
        actionLabel: '新建设备',
        actionHint: '立即保存',
        pending: true,
      },
      attachTo: document.body,
    })
    await new Promise((resolve) => setTimeout(resolve, 0))
    const dialog = document.body.querySelector('[role="dialog"]') as HTMLElement
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    const buttons = dialog.querySelectorAll<HTMLButtonElement>('button')
    expect(buttons[0]?.disabled).toBe(true)
    expect(buttons[buttons.length - 1]?.disabled).toBe(true)
    expect(wrapper.emitted('update:open')).toBeUndefined()
    expect(dialog.contains(document.activeElement)).toBe(true)
    expect(document.activeElement?.getAttribute('data-testid')).toBe('credential-receipt-token')
    wrapper.unmount()
  })
})
