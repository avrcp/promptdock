import { flushPromises, mount } from '@vue/test-utils'
import { createMemoryHistory, createRouter } from 'vue-router'
import { describe, expect, it } from 'vitest'

import NotFoundPage from './NotFoundPage.vue'

async function mountPage(): Promise<{
  router: ReturnType<typeof createRouter>
  wrapper: ReturnType<typeof mount>
}> {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/overview', name: 'overview', component: { template: '<div />' } },
      { path: '/:pathMatch(.*)*', name: 'not-found', component: NotFoundPage },
    ],
  })
  await router.push('/missing/operator-view?source=test')
  await router.isReady()
  const wrapper = mount(NotFoundPage, { global: { plugins: [router] } })
  return { router, wrapper }
}

describe('NotFoundPage', () => {
  it('shows the requested path without a decorative 404 number', async () => {
    const { wrapper } = await mountPage()

    expect(wrapper.text()).toContain('页面不存在')
    expect(wrapper.get('[data-testid="not-found-path"]').text()).toBe(
      '/missing/operator-view?source=test',
    )
    expect(wrapper.find('.not-found__code').exists()).toBe(false)
    wrapper.unmount()
  })

  it('returns to the overview route', async () => {
    const { router, wrapper } = await mountPage()

    await wrapper.get('[data-testid="not-found-home"]').trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.fullPath).toBe('/overview')
    wrapper.unmount()
  })
})
