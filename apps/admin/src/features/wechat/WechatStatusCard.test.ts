import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import type { WechatAdminStatus } from '@/contracts/wechat'
import WechatStatusCard from './WechatStatusCard.vue'

const status: WechatAdminStatus = {
  schemaVersion: 2,
  generatedAt: 1,
  state: 'disconnected',
  accountHint: null,
  lastPollAt: null,
  lastContextAt: null,
  lastProviderAcceptedAt: null,
  queue: { pending: 0, sending: 0, retrying: 0, blocked: 0, failed: 0 },
  lastErrorCode: null,
}

describe('WechatStatusCard', () => {
  it('renders an explicit placeholder for every unavailable timestamp', () => {
    const wrapper = mount(WechatStatusCard, {
      props: {
        wechat: status,
        statusVisual: { tone: 'muted', label: '未连接' },
      },
    })

    const timestampValues = wrapper.findAll('.wechat-status__detail dd').slice(0, 3)
    expect(timestampValues).toHaveLength(3)
    expect(timestampValues.map((value) => value.text())).toEqual(['—', '—', '—'])
  })
})
