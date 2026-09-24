import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'

import AppPagination from './AppPagination.vue'

describe('AppPagination', () => {
  it('announces the known result range and emits explicit paging actions', async () => {
    const wrapper = mount(AppPagination, {
      props: { page: 2, pageSize: 25, total: 63, hasNext: true, hasPrev: true },
    })

    expect(wrapper.get('[aria-live="polite"]').text()).toBe('第 26–50 条 / 共 63 条')
    const buttons = wrapper.findAll('button')
    await buttons[0]?.trigger('click')
    await buttons[1]?.trigger('click')
    expect(wrapper.emitted('prev')).toHaveLength(1)
    expect(wrapper.emitted('next')).toHaveLength(1)
    wrapper.unmount()
  })

  it('keeps unavailable directions disabled and describes unknown totals safely', () => {
    const wrapper = mount(AppPagination, {
      props: { page: 3, pageSize: 20, total: null, hasNext: false, hasPrev: false },
    })

    expect(wrapper.get('[aria-live="polite"]').text()).toBe('第 3 页 · 每页 20')
    for (const button of wrapper.findAll('button'))
      expect(button.attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('shows "共 0 条" when total is zero instead of an invalid range', () => {
    const wrapper = mount(AppPagination, {
      props: { page: 1, pageSize: 25, total: 0, hasNext: false, hasPrev: false },
    })

    expect(wrapper.get('[aria-live="polite"]').text()).toBe('共 0 条')
    wrapper.unmount()
  })

  it('computes the correct range on the last page with a remainder', () => {
    const wrapper = mount(AppPagination, {
      props: { page: 3, pageSize: 25, total: 63, hasNext: false, hasPrev: true },
    })

    expect(wrapper.get('[aria-live="polite"]').text()).toBe('第 51–63 条 / 共 63 条')
    wrapper.unmount()
  })
})
