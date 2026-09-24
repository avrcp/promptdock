import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import WechatEventTable from './WechatEventTable.vue'

describe('WechatEventTable', () => {
  it('preserves the empty-state and safe event projection', async () => {
    const wrapper = mount(WechatEventTable, {
      props: { events: [], loading: false },
    })
    expect(wrapper.text()).toContain('没有通道事件')

    await wrapper.setProps({
      events: [
        {
          id: 'event-1',
          kind: 'login',
          occurredAt: Date.now() - 1_000,
          safeMessage: '微信登录流程已开始',
        },
      ],
    })
    expect(wrapper.text()).toContain('login')
    expect(wrapper.text()).toContain('微信登录流程已开始')
    expect(wrapper.find('table').exists()).toBe(true)
    expect(wrapper.findAll('th')).toHaveLength(3)
  })
})
