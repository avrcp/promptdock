import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { nextTick, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import type { Delivery } from '../desktop/dictionaries'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
const mockedInvoke = vi.mocked(invoke)

const delivery: Delivery = {
  id: 'delivery-1',
  status: 'delivered',
  remoteStatus: 'provider_accepted',
  createdAt: Date.now(),
  payload: { title: '完整最终回答' },
  result: {
    resultId: 'delivery-1',
    sourceHash: 'a'.repeat(64),
    pageState: 'available',
    pageExpiresAt: Date.now() + 86_400_000,
    notificationId: 'notice-1',
    notificationStatus: 'provider_accepted',
  },
}

async function mountView(options?: { nextCursor?: string | null }) {
  const state = {
    deliveries: ref<Delivery[]>([delivery]),
    deliveriesNextCursor: ref(options?.nextCursor ?? null),
    deliveriesPagePending: ref(false),
    navigate: vi.fn(),
    refreshDeliveries: vi.fn(async () => undefined),
    loadMoreDeliveries: vi.fn(async () => undefined),
  }
  vi.doMock('../desktop/desktopState', () => ({ useDesktopState: () => state }))
  const { default: DeliveriesView } = await import('./DeliveriesView.vue')
  return { wrapper: mount(DeliveriesView, { attachTo: document.body }), state }
}

afterEach(() => {
  vi.clearAllMocks()
  vi.resetModules()
  document.body.innerHTML = ''
})

describe('DeliveriesView', () => {
  it('从列表直接打开服务器结果，不先读取本地正文', async () => {
    mockedInvoke.mockImplementation(async (command) => {
      if (command === 'desktop_delivery_metadata') return delivery
      return undefined
    })
    const { wrapper } = await mountView()
    await wrapper.get('button[data-action="open-result"]').trigger('click')
    await flushPromises()

    expect(mockedInvoke).toHaveBeenCalledWith('desktop_result_open', { id: delivery.id })
    expect(mockedInvoke).not.toHaveBeenCalledWith('desktop_delivery_detail', expect.anything())
    expect(wrapper.get('.result-summary').text()).toContain('服务器结果页')
    wrapper.unmount()
  })

  it('本地正文读取失败不会移除服务器结果与链接操作', async () => {
    mockedInvoke.mockImplementation(async (command) => {
      if (command === 'desktop_delivery_metadata') return delivery
      if (command === 'desktop_delivery_detail') throw new Error('DPAPI unavailable')
      return undefined
    })
    const { wrapper } = await mountView()
    await wrapper.get('button[data-action="local-body"]').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('本地原文暂不可读取')
    expect(wrapper.get('.result-summary').text()).toContain('查看结果')
    wrapper.unmount()
  })

  it('撤销必须经过说明影响的确认对话框', async () => {
    mockedInvoke.mockImplementation(async (command) => {
      if (command === 'desktop_delivery_metadata') return delivery
      return undefined
    })
    const { wrapper } = await mountView()
    await wrapper.get('button[data-action="open-result"]').trigger('click')
    await flushPromises()
    const revoke = wrapper
      .findAll('button')
      .find((button) => button.text() === '撤销链接' && !button.attributes('data-action'))!
    await revoke.trigger('click')
    await flushPromises()

    const dialog = document.querySelector<HTMLDialogElement>('dialog')!
    expect(dialog.open).toBe(true)
    expect(dialog.textContent).toContain('微信中的旧通知不会消失')
    const confirm = Array.from(dialog.querySelectorAll('button')).find(
      (button) => button.textContent?.trim() === '撤销链接',
    )!
    confirm.click()
    await flushPromises()
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_result_revoke', {
      id: delivery.id,
      requestId: expect.any(String),
    })
    wrapper.unmount()
  })

  it('存在 next cursor 时提供有界加载更多入口', async () => {
    mockedInvoke.mockResolvedValue(delivery)
    const { wrapper, state } = await mountView({ nextCursor: 'cursor-2' })
    const button = wrapper.findAll('button').find((item) => item.text() === '加载更多')!
    await button.trigger('click')
    expect(state.loadMoreDeliveries).toHaveBeenCalledOnce()
    wrapper.unmount()
  })

  it('较早选中项的元数据晚到时不会覆盖当前选中 ID', async () => {
    let resolveFirst!: (item: Delivery) => void
    const firstMetadata = new Promise<Delivery>((resolve) => {
      resolveFirst = resolve
    })
    const second: Delivery = {
      ...delivery,
      id: 'delivery-2',
      payload: { title: '第二条结果' },
      result: { ...delivery.result!, resultId: 'delivery-2' },
    }
    mockedInvoke.mockImplementation(async (command, args) => {
      const id = (args as { id?: string } | undefined)?.id
      if (command === 'desktop_delivery_metadata')
        return id === delivery.id ? firstMetadata : second
      if (command === 'desktop_delivery_detail')
        return {
          id,
          title: id === delivery.id ? delivery.payload.title : second.payload.title,
          body: 'body',
          contentMode: 'full_final',
          contentBytes: 4,
          sourceHash: 'a'.repeat(64),
          unavailableReason: null,
        }
      return undefined
    })
    const { wrapper, state } = await mountView()
    state.deliveries.value.push(second)
    await nextTick()
    const localButtons = wrapper.findAll('button[data-action="local-body"]')
    await localButtons[0]!.trigger('click')
    await localButtons[1]!.trigger('click')
    await flushPromises()
    resolveFirst(delivery)
    await flushPromises()

    expect(wrapper.get('tr.data-table__row--selected .data-table__title').text()).toBe('第二条结果')
    expect(wrapper.findAll('.panel__title').at(-1)?.text()).toBe('第二条结果')
    wrapper.unmount()
  })

  it('目的地变化按确定错误反馈，不称为结果未知', async () => {
    mockedInvoke.mockImplementation(async (command) => {
      if (command === 'desktop_delivery_metadata') return delivery
      if (command === 'desktop_delivery_detail')
        return {
          id: delivery.id,
          title: delivery.payload.title,
          body: null,
          contentMode: 'full_final',
          contentBytes: 0,
          sourceHash: delivery.result?.sourceHash ?? null,
          unavailableReason: '本地原文不可用',
        }
      if (command === 'desktop_result_resend') throw { code: 'RESULT_DESTINATION_CHANGED' }
      return undefined
    })
    const { wrapper } = await mountView()
    await wrapper.get('button[data-action="local-body"]').trigger('click')
    await flushPromises()
    const resend = wrapper.findAll('button').find((button) => button.text() === '重新通知')!
    await resend.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('已阻止转投当前设备')
    expect(wrapper.text()).not.toContain('操作结果尚未确认')
    wrapper.unmount()
  })

  it('服务器确认结果不存在时不提示继续按未知结果重试', async () => {
    mockedInvoke.mockImplementation(async (command) => {
      if (command === 'desktop_delivery_metadata') return delivery
      if (command === 'desktop_delivery_detail')
        return {
          id: delivery.id,
          title: delivery.payload.title,
          body: null,
          contentMode: 'full_final',
          contentBytes: 0,
          sourceHash: delivery.result?.sourceHash ?? null,
          unavailableReason: '本地原文不可用',
        }
      if (command === 'desktop_result_resend')
        throw new Error('RELAY_RESULT_NOT_FOUND: result unavailable')
      return undefined
    })
    const { wrapper } = await mountView()
    await wrapper.get('button[data-action="local-body"]').trigger('click')
    await flushPromises()
    await wrapper
      .findAll('button')
      .find((button) => button.text() === '重新通知')!
      .trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('重试不会恢复它')
    expect(wrapper.text()).not.toContain('操作结果尚未确认')
    wrapper.unmount()
  })
})
