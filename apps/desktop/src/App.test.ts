import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { invoke, type InvokeArgs } from '@tauri-apps/api/core'
import App from './App.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
const mockedInvoke = vi.mocked(invoke)
const hookPlan = {
  registrationId: 'd03aa6bb-ec9f-4f4c-af2f-45ad18d2856e',
  operationId: '5a58deab-15ec-4815-930e-cb81800ef961',
  sourceFingerprint: null,
  expectedPolicy: {
    observe_turns: true,
    notify_started: true,
    notify_ended: true,
    completion_quiet_ms: 2000,
    result_content_mode: 'status_only',
    notify_attention: false,
    include_task_input: false,
  },
  changedEvents: ['UserPromptSubmit', 'Stop'],
  definitionChanged: true,
  outcome: 'installed',
  reviewRequired: true,
  externalReviewRequired: true,
}
const invokeDesktopMock = async (command: string, args?: InvokeArgs) => {
  if (command === 'desktop_status')
    return {
      dataDirectory: 'C:\\Users\\example\\.promptdock-desktop',
      policy: {
        observe_turns: true,
        notify_started: true,
        notify_ended: true,
        completion_quiet_ms: 2000,
        result_content_mode: 'status_only',
        notify_attention: false,
        include_task_input: false,
      },
      policyRevision: 0,
      policyApplyStatus: 'saved',
      hookHome: null,
      hookState: 'not_configured',
      hookScope: '仅当前配置目录',
      inboxPresent: false,
    }
  if (command === 'desktop_launcher_status')
    return {
      config: {
        proxy: { enabled: false, host: '127.0.0.1', port: 7890, noProxy: [] },
        desktop: { selectedExecutable: null, refuseIfRunning: true },
      },
      candidates: [],
      selectedRunning: false,
      discoveryIssue: null,
      selectionIssue: null,
    }
  if (command === 'desktop_relay_status')
    return {
      runtimeEpoch: 'test-epoch',
      revision: 1,
      state: 'not_configured',
      canSubmit: null,
      canReadOwn: null,
      configured: false,
      reachable: false,
      authenticated: false,
      lastErrorCode: null,
    }
  if (command === 'desktop_hook_health')
    return {
      runtimeEpoch: 'test-epoch',
      revision: 1,
      observedAt: Date.now(),
      fresh: true,
      observationEnabled: true,
      hookSourcePath: null,
      userStateSourcePath: null,
      sourceResolution: 'unknown',
      compatibility: 'unverified',
      hostBuild: null,
      installation: 'absent',
      registrationId: null,
      definitionFingerprint: null,
      diagnosticCode: null,
      handlers: [],
      verification: {
        state: 'not_started',
        verificationId: null,
        validatedAt: null,
        validatedDefinitionFingerprint: null,
        hostAttribution: 'unknown',
        instruction: null,
      },
    }
  if (command === 'desktop_hook_plan' || command === 'desktop_install_hook') return hookPlan
  if (command === 'desktop_launcher_save')
    return (args as Record<string, unknown> | undefined)?.config
  if (command === 'desktop_save_policy')
    return {
      status: 'saved',
      state: {
        policy: (args as Record<string, unknown> | undefined)?.policy,
        captureGeneration: 0,
        revision: 1,
      },
    }
  if (command === 'desktop_deliveries') return { items: [], nextCursor: null }
  if (command === 'desktop_autostart_status') return false
  return null
}
beforeEach(() => {
  mockedInvoke.mockReset()
  mockedInvoke.mockImplementation(invokeDesktopMock)
})
describe('desktop settings UI (mocked command boundary)', () => {
  it('defaults to state-only and requires explicit host and hook selection', async () => {
    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('button[aria-label="通知（Relay 与通知）"]').trigger('click')
    expect(wrapper.get('#policy-content-mode').element).toHaveProperty('value', 'status_only')
    await wrapper.get('#policy-content-mode').setValue('full_final')
    expect(wrapper.text()).toContain('持有链接的人可以阅读该结果')
    await wrapper.get('button[aria-label="集成（桌面与 Hook）"]').trigger('click')
    const plan = wrapper.findAll('button').find((b) => b.text() === '检查变更')!
    expect(plan.attributes('disabled')).toBeDefined()
    await wrapper.get('#hook-home').setValue('C:\\isolated-codex')
    await plan.trigger('click')
    await flushPromises()
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_hook_plan', {
      home: 'C:\\isolated-codex',
    })
    expect(wrapper.text()).toContain('持久信任记录与实际接收事实分别显示')
    const apply = wrapper.findAll('button').find((b) => b.text() === '应用 Hook 变更')!
    await apply.trigger('click')
    await flushPromises()
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_install_hook', {
      home: 'C:\\isolated-codex',
      registrationId: hookPlan.registrationId,
      expectedFingerprint: null,
      expectedPolicy: hookPlan.expectedPolicy,
    })
    wrapper.unmount()
  })
  it('reports command failure without claiming success', async () => {
    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('button[aria-label="通知（Relay 与通知）"]').trigger('click')
    await wrapper.get('#policy-started').setValue(false)
    mockedInvoke.mockImplementation(async (command, args) => {
      if (command === 'desktop_save_policy') throw '凭据验证失败'
      return invokeDesktopMock(command, args)
    })
    await wrapper
      .findAll('button')
      .find((b) => b.text() === '保存通知设置')!
      .trigger('click')
    await flushPromises()
    expect(wrapper.get('.toast-region').text()).toBe('凭据验证失败')
    wrapper.unmount()
  })
  it('exposes the bounded completion quiet window and preserves it on save', async () => {
    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('button[aria-label="通知（Relay 与通知）"]').trigger('click')
    const quiet = wrapper.get('#policy-completion-quiet-ms')
    expect(quiet.attributes('min')).toBe('500')
    expect(quiet.attributes('max')).toBe('20000')
    expect(quiet.element).toHaveProperty('value', '2000')

    await quiet.setValue('5000')
    await wrapper
      .findAll('button')
      .find((button) => button.text() === '保存通知设置')!
      .trigger('click')
    await flushPromises()
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_save_policy', {
      policy: expect.objectContaining({ completion_quiet_ms: 5000 }),
      expectedRevision: 0,
    })
    wrapper.unmount()
  })
  it('exposes five truthful navigation destinations', async () => {
    const wrapper = mount(App)
    await flushPromises()
    expect(wrapper.findAll('nav[aria-label="主导航"] button.navigation-item')).toHaveLength(5)
    await wrapper.get('button[aria-label="投递（投递记录）"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('h1').text()).toBe('投递记录')
    expect(wrapper.text()).toContain('暂无投递')
    wrapper.unmount()
  })
  it('renders the latest delivery and distinguishes provider acceptance', async () => {
    mockedInvoke.mockImplementation(async (command) => {
      if (command === 'desktop_deliveries')
        return {
          items: [
            {
              id: 'notification-1',
              status: 'delivered',
              remoteStatus: 'provider_accepted',
              createdAt: 1_788_970_030,
              payload: { title: '任务已结束' },
            },
          ],
          nextCursor: null,
        }
      return invokeDesktopMock(command)
    })
    const wrapper = mount(App)
    await flushPromises()
    expect(wrapper.text()).toContain('任务已结束')
    expect(wrapper.text()).toContain('微信接口已接收（手机显示待核验）')
    wrapper.unmount()
  })
  it('keeps available desktop state when one optional status read fails', async () => {
    mockedInvoke.mockImplementation(async (command) => {
      if (command === 'desktop_relay_status') throw new Error('relay unavailable')
      return invokeDesktopMock(command)
    })
    const wrapper = mount(App)
    await flushPromises()
    expect(wrapper.text()).toContain('部分状态暂时无法读取；已显示当前可用信息。')
    expect(wrapper.text()).toContain('未检测到运行中的所选宿主')
    expect(wrapper.text()).not.toContain('本机状态不可用')
    wrapper.unmount()
  })
  it('loads full body only on explicit detail action and releases it on close', async () => {
    const body = '  final-body-sentinel\r\n```text\n  preserved\n```'
    mockedInvoke.mockImplementation(async (command) => {
      if (command === 'desktop_deliveries')
        return {
          items: [
            {
              id: 'bundle-1',
              status: 'delivered',
              remoteStatus: 'retry_wait',
              createdAt: 1,
              payload: { title: '最终回答' },
              result: {
                resultId: 'bundle-1',
                sourceHash: 'a'.repeat(64),
                pageState: 'available',
                pageExpiresAt: Date.now() + 86400000,
                notificationId: 'notice-1',
                notificationStatus: 'retry_wait',
              },
            },
          ],
          nextCursor: null,
        }
      if (command === 'desktop_delivery_detail')
        return {
          id: 'bundle-1',
          title: '最终回答',
          body,
          contentMode: 'full_final',
          contentBytes: body.length,
          sourceHash: null,
          unavailableReason: null,
        }
      return invokeDesktopMock(command)
    })
    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('button[aria-label="投递（投递记录）"]').trigger('click')
    expect(wrapper.text()).not.toContain('final-body-sentinel')
    expect(mockedInvoke.mock.calls.some(([command]) => command === 'desktop_delivery_detail')).toBe(
      false,
    )
    expect(wrapper.text()).toContain('结果页：可查看至')
    expect(wrapper.text()).toContain('微信通知：等待重试')
    expect(wrapper.text()).not.toContain('段')
    await wrapper
      .findAll('button')
      .find((button) => button.text() === '查看本地原文')!
      .trigger('click')
    await flushPromises()
    expect(wrapper.get('pre[aria-label="通知正文"]').element.textContent).toBe(body)
    await wrapper
      .findAll('button')
      .find((button) => button.text() === '关闭详情')!
      .trigger('click')
    expect(wrapper.find('pre[aria-label="通知正文"]').exists()).toBe(false)
    wrapper.unmount()
  })
  it('opens server results without reading local body and supplies request identities for resend', async () => {
    let resendCalls = 0
    mockedInvoke.mockImplementation(async (command) => {
      if (command === 'desktop_deliveries')
        return {
          items: [
            {
              id: 'result-1',
              status: 'delivered',
              remoteStatus: 'retry_wait',
              createdAt: Date.now(),
              payload: { title: 'Codex 最终回答' },
              result: {
                resultId: 'result-1',
                sourceHash: 'a'.repeat(64),
                pageState: 'available',
                pageExpiresAt: Date.now() + 86400000,
                notificationId: 'notice-1',
                notificationStatus: 'retry_wait',
              },
            },
          ],
        }
      if (command === 'desktop_delivery_detail')
        return {
          id: 'result-1',
          title: 'Codex 最终回答',
          body: 'exact body',
          contentMode: 'full_final',
          contentBytes: 10,
          sourceHash: 'a'.repeat(64),
          unavailableReason: null,
        }
      if (command === 'desktop_delivery_metadata')
        return {
          id: 'result-1',
          status: 'delivered',
          remoteStatus: 'retry_wait',
          createdAt: Date.now(),
          payload: { title: 'Codex 最终回答' },
          result: {
            resultId: 'result-1',
            sourceHash: 'a'.repeat(64),
            pageState: 'available',
            pageExpiresAt: Date.now() + 86400000,
            notificationId: 'notice-1',
            notificationStatus: 'retry_wait',
          },
        }
      if (command === 'desktop_result_resend' && ++resendCalls === 1)
        throw new Error('response lost')
      return invokeDesktopMock(command)
    })
    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('button[aria-label="投递（投递记录）"]').trigger('click')
    await wrapper
      .findAll('button')
      .find((button) => button.text() === '查看结果')!
      .trigger('click')
    await flushPromises()
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_result_open', { id: 'result-1' })
    expect(mockedInvoke.mock.calls.some(([name]) => name === 'desktop_delivery_detail')).toBe(false)
    const resend = wrapper.findAll('button').find((button) => button.text() === '重新通知')!
    await resend.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('操作结果尚未确认')
    await resend.trigger('click')
    await flushPromises()
    const attempts = mockedInvoke.mock.calls.filter(([name]) => name === 'desktop_result_resend')
    expect(attempts).toHaveLength(2)
    expect(attempts[0]![1]).toEqual({ id: 'result-1', requestId: expect.any(String) })
    expect(attempts[1]![1]).toEqual({ id: 'result-1', requestId: expect.any(String) })
    expect(wrapper.text()).toContain('重新通知已由服务器接管')
    wrapper.unmount()
  })
  it('confirms before revoking a server result link', async () => {
    const item = {
      id: 'result-revoke',
      status: 'delivered',
      remoteStatus: 'provider_accepted',
      createdAt: Date.now(),
      payload: { title: '待撤销结果' },
      result: {
        resultId: 'result-revoke',
        sourceHash: 'b'.repeat(64),
        pageState: 'available',
        pageExpiresAt: Date.now() + 86400000,
        notificationId: 'notice-revoke',
        notificationStatus: 'provider_accepted',
      },
    }
    mockedInvoke.mockImplementation(async (command, args) => {
      if (command === 'desktop_deliveries') return { items: [item], nextCursor: null }
      if (command === 'desktop_delivery_metadata') return item
      return invokeDesktopMock(command, args)
    })
    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('button[aria-label="投递（投递记录）"]').trigger('click')
    await wrapper.find('[data-action="open-result"]').trigger('click')
    await flushPromises()
    await wrapper
      .findAll('button')
      .find((button) => button.text() === '撤销链接')!
      .trigger('click')
    await flushPromises()

    expect(mockedInvoke.mock.calls.some(([name]) => name === 'desktop_result_revoke')).toBe(false)
    const dialog = document.querySelector('[role="alertdialog"]')!
    expect(dialog.textContent).toContain('撤销后此链接不再可读；微信中的旧通知不会消失')
    const confirm = Array.from(dialog.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('撤销链接'),
    )!
    confirm.click()
    await flushPromises()
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_result_revoke', {
      id: 'result-revoke',
      requestId: expect.any(String),
    })
    wrapper.unmount()
  })
  it('requires confirmation before uninstalling the hook', async () => {
    mockedInvoke.mockImplementation(async (command: string) => {
      const base = (await invokeDesktopMock(command)) as Record<string, unknown> | null
      if (command === 'desktop_status')
        return { ...(base ?? {}), hookHome: 'C:\\isolated-codex', hookState: 'configured' }
      return base
    })
    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('button[aria-label="集成（桌面与 Hook）"]').trigger('click')
    const uninstall = wrapper.findAll('button').find((b) => b.text() === '卸载')!
    expect(uninstall.attributes('disabled')).toBeUndefined()

    await uninstall.trigger('click')
    await flushPromises()
    const dialog = document.querySelector('[role="alertdialog"]')
    expect(dialog).not.toBeNull()
    expect(mockedInvoke).not.toHaveBeenCalledWith('desktop_uninstall_hook')

    const cancel = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="alertdialog"] button'),
    ).find((b) => b.textContent?.includes('取消'))!
    cancel.click()
    await flushPromises()
    expect(document.querySelector('[role="alertdialog"]')).toBeNull()
    expect(mockedInvoke).not.toHaveBeenCalledWith('desktop_uninstall_hook')
    wrapper.unmount()
  })
  it('renders deliveries as a native table with column headers', async () => {
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_deliveries')
        return {
          items: [
            {
              id: 'notification-1',
              status: 'delivered',
              remoteStatus: 'provider_accepted',
              createdAt: 1_788_970_030,
              payload: { title: '任务已结束' },
            },
          ],
          nextCursor: null,
        }
      return invokeDesktopMock(command)
    })
    const wrapper = mount(App)
    await flushPromises()
    expect(wrapper.find('table.data-table').exists()).toBe(true)
    expect(wrapper.findAll('table.data-table th[scope="col"]')).toHaveLength(3)
    expect(wrapper.get('table.data-table time').attributes('datetime')).toBeDefined()
    expect(wrapper.get('table.data-table caption').classes()).toContain('sr-only')
    wrapper.unmount()
  })
  it('keeps truncated values reachable', async () => {
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_relay_status')
        return {
          baseUrl: 'https://relay.example.internal',
          runtimeEpoch: 'epoch-test',
          revision: 1,
          state: 'ready',
          canSubmit: true,
          canReadOwn: true,
          configured: true,
          reachable: true,
          authenticated: true,
          lastErrorCode: null,
        }
      return invokeDesktopMock(command)
    })
    const wrapper = mount(App)
    await flushPromises()
    const titles = wrapper.findAll('.status-card small[title]').map((n) => n.attributes('title'))
    expect(titles).toContain('https://relay.example.internal')
    expect(wrapper.get('.global-status__path').attributes('title')).toContain('promptdock-desktop')
    wrapper.unmount()
  })
  it('keeps a stable polite status region for repeated announcements', async () => {
    const wrapper = mount(App)
    await flushPromises()
    const region = wrapper.get('[role="status"]')
    expect(region.text()).toBe('')
    wrapper.unmount()
  })
  it('makes unavailable desktop runtime explicit', async () => {
    mockedInvoke.mockRejectedValue(new Error('unavailable'))
    const wrapper = mount(App)
    await flushPromises()
    expect(wrapper.text()).toContain('请在 PromptDock Desktop 应用内使用设置')
    wrapper.unmount()
  })
})
