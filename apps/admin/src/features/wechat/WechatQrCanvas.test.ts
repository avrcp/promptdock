import { flushPromises, mount } from '@vue/test-utils'
import QRCode from 'qrcode'
import { afterEach, describe, expect, it, vi } from 'vitest'

import WechatQrCanvas from './WechatQrCanvas.vue'

describe('WechatQrCanvas', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders a QR SVG without exposing its payload to the document', async () => {
    const toString = vi.spyOn(QRCode, 'toString')
    const payload = 'sensitive-qr-payload-must-not-be-rendered'
    const wrapper = mount(WechatQrCanvas, { props: { content: payload } })
    await flushPromises()

    expect(wrapper.find('svg').exists()).toBe(true)
    expect(toString).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).not.toContain(payload)
    expect(wrapper.html()).not.toContain(payload)

    await wrapper.setProps({ content: null })
    await flushPromises()
    expect(wrapper.find('svg').exists()).toBe(false)

    wrapper.unmount()
  })
})
