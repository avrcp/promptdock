import { mount } from '@vue/test-utils'
import { defineComponent, h } from 'vue'
import { describe, expect, it } from 'vitest'
import NotificationHoldPanel from './NotificationHoldPanel.vue'
import { provideNotificationHoldController } from '../desktop/notificationHold'
const Host = defineComponent({
  setup() {
    provideNotificationHoldController()
    return () => h(NotificationHoldPanel)
  },
})
describe('NotificationHoldPanel', () => {
  it('states the durable-content boundary and keeps the unknown state recoverable', () => {
    const wrapper = mount(Host)
    expect(wrapper.text()).toContain('按已选内容策略继续保存本机记录')
    expect(wrapper.findAll('button').map((b) => b.text())).toEqual(['恢复通知'])
    wrapper.unmount()
  })
})
