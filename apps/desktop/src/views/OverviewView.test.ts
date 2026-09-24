import { mount } from '@vue/test-utils'
import { defineComponent, h } from 'vue'
import { describe, expect, it } from 'vitest'
import { provideActivityController } from '../desktop/activity'
import { provideDesktopState } from '../desktop/desktopState'
import OverviewView from './OverviewView.vue'

const Host = defineComponent({
  setup() {
    provideDesktopState()
    provideActivityController()
    return () => h(OverviewView)
  },
})

describe('OverviewView activity summary', () => {
  it('renders the five keyboard-operable metadata filters and a truthful empty state', () => {
    const wrapper = mount(Host)
    const filters = wrapper.findAll('[aria-label="活动过滤"] button')
    expect(filters).toHaveLength(5)
    expect(filters.every((button) => button.attributes('type') === 'button')).toBe(true)
    expect(wrapper.text()).toContain('当前没有此类活动')
    expect(wrapper.text()).toContain('不会显示原始任务内容')
    wrapper.unmount()
  })
})
