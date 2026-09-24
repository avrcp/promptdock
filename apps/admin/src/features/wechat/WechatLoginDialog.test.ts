import { mount } from '@vue/test-utils'
import { afterEach, describe, expect, it } from 'vitest'

import WechatLoginDialog from './WechatLoginDialog.vue'

const verifySession = {
  loginId: 'login-1',
  state: 'verify_code_required' as const,
  expiresAt: Date.now() + 60_000,
  canSubmitVerifyCode: true,
}

describe('WechatLoginDialog', () => {
  afterEach(() => {
    document.body.innerHTML = ''
  })

  it('keeps the verify code owned by its caller and emits only explicit actions', async () => {
    const wrapper = mount(WechatLoginDialog, {
      props: {
        open: true,
        session: verifySession,
        state: 'verify_code_required',
        error: null,
        inFlight: false,
        secondsLeft: 60,
        verifyCode: '',
        canStartLogin: true,
      },
      attachTo: document.body,
    })

    const input = document.body.querySelector(
      '[data-testid="wechat-login-verify-code"]',
    ) as HTMLInputElement | null
    expect(input).not.toBeNull()
    expect(document.body.querySelector('[data-testid="wechat-qr"]')).toBeNull()
    input!.value = '123456'
    input!.dispatchEvent(new Event('input', { bubbles: true }))
    await wrapper.vm.$nextTick()
    expect(wrapper.emitted('update:verifyCode')?.[0]).toEqual(['123456'])

    const form = document.body.querySelector('form')
    form?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
    expect(wrapper.emitted('verify')).toHaveLength(1)
    wrapper.unmount()
  })
})
