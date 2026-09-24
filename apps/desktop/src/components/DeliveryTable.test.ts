import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import DeliveryTable from './DeliveryTable.vue'
import type { Delivery } from '../desktop/dictionaries'

const delivery: Delivery = {
  id: 'delivery-1',
  status: 'delivered',
  remoteStatus: 'provider_accepted',
  createdAt: 1_789_000_000_000,
  payload: { title: '完整最终回答' },
  result: {
    resultId: 'delivery-1',
    sourceHash: 'a'.repeat(64),
    pageState: 'available',
    pageExpiresAt: 1_999_000_000_000,
    notificationId: 'notice-1',
    notificationStatus: 'provider_accepted',
  },
}

describe('DeliveryTable', () => {
  it('把服务器结果与本地原文作为独立动作暴露', async () => {
    const wrapper = mount(DeliveryTable, {
      props: { items: [delivery], caption: '投递记录', showId: true },
    })
    const buttons = wrapper.findAll('button')
    expect(buttons.map((button) => button.text())).toEqual(['查看结果', '查看本地原文'])

    await buttons[0]!.trigger('click')
    await buttons[1]!.trigger('click')
    expect(wrapper.emitted('openResult')).toEqual([[delivery]])
    expect(wrapper.emitted('localBody')).toEqual([[delivery]])
  })

  it('结果页不可用时仍允许单独查看本地原文', () => {
    const wrapper = mount(DeliveryTable, {
      props: {
        items: [{ ...delivery, result: { ...delivery.result!, pageState: 'revoked' } }],
        caption: '投递记录',
        showId: true,
      },
    })
    expect(wrapper.find('button[data-action="open-result"]').attributes('disabled')).toBeDefined()
    expect(wrapper.find('button[data-action="local-body"]').attributes('disabled')).toBeUndefined()
  })

  it('结果页时间已过时不提供看似可用的直接入口', () => {
    const wrapper = mount(DeliveryTable, {
      props: {
        items: [
          {
            ...delivery,
            result: { ...delivery.result!, pageState: 'available', pageExpiresAt: Date.now() - 1 },
          },
        ],
        caption: '投递记录',
        showId: true,
      },
    })
    expect(wrapper.get('button[data-action="open-result"]').attributes('disabled')).toBeDefined()
  })

  it('用选中 ID 标记稳定选中行', () => {
    const wrapper = mount(DeliveryTable, {
      props: { items: [delivery], caption: '投递记录', showId: true, selectedId: delivery.id },
    })
    expect(wrapper.get('tr.data-table__row--selected .data-table__selection').text()).toBe('当前详情')
  })
})
