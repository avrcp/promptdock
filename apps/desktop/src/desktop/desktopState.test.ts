import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises } from '@vue/test-utils'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import {
  createDesktopState,
  type LauncherStatus,
  type Policy,
  type RelayStatus,
  type RelayStatusCore,
} from './desktopState'
import type { HookHealth } from './hookHealth'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }))
const mockedInvoke = vi.mocked(invoke)
const mockedListen = vi.mocked(listen)

const policy = (): Policy => ({ observe_turns: true, notify_started: true, notify_ended: true, completion_quiet_ms: 2000, result_content_mode: 'status_only', notify_attention: false, include_task_input: false })
const health = (overrides: Partial<HookHealth> = {}): HookHealth => ({ runtimeEpoch: 'health', revision: 1, observedAt: 1, fresh: true, observationEnabled: true, hookSourcePath: 'C:\\codex\\hooks.json', userStateSourcePath: null, sourceResolution: 'confirmed', compatibility: 'validated', hostBuild: 'build', installation: 'current', registrationId: 'registration', definitionFingerprint: 'fingerprint', diagnosticCode: null, handlers: [{ event: 'UserPromptSubmit', required: true, configured: true, trust: 'matching_record', evidenceSource: 'user', lastErrorCode: null, lastObservedAt: null, observedRegistrationId: null, observationCurrent: false }], verification: { state: 'not_started', verificationId: null, validatedAt: null, validatedDefinitionFingerprint: null, hostAttribution: 'unknown', instruction: null }, ...overrides })
const launcher = (overrides: Partial<LauncherStatus> = {}): LauncherStatus => ({ config: { proxy: { enabled: false, host: '127.0.0.1', port: 7890, noProxy: [] }, desktop: { selectedExecutable: null, refuseIfRunning: true } }, candidates: [], selectedRunning: false, discoveryIssue: null, selectionIssue: null, ...overrides })
const relay = (overrides: Partial<RelayStatus> = {}): RelayStatus => ({ runtimeEpoch: 'relay', revision: 1, state: 'ready', canSubmit: true, canReadOwn: true, configured: true, reachable: true, authenticated: true, lastErrorCode: null, baseUrl: 'https://relay.example.test', ...overrides })
const status = (overrides: Record<string, unknown> = {}) => ({ dataDirectory: 'C:\\isolated', policy: policy(), policyRevision: 4, policyApplyStatus: 'saved', hookHome: null, inboxPresent: true, ...overrides })
const delivery = (id: string, updatedAt: number, itemStatus = 'pending') => ({ id, status: itemStatus, remoteStatus: null, createdAt: updatedAt, updatedAt, payload: { title: id } })

function defaults(command: string): unknown {
  return ({ desktop_status: status(), desktop_hook_health: health(), desktop_launcher_status: launcher(), desktop_relay_status: relay(), desktop_autostart_status: false, desktop_deliveries: { items: [], nextCursor: null } })[command]
}
function installMock() { mockedInvoke.mockImplementation(async (command: string) => defaults(command) as never) }
beforeEach(() => { mockedInvoke.mockReset(); mockedListen.mockReset(); mockedListen.mockResolvedValue(() => undefined); Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' }) })

describe('shared Hook health controller', () => {
  it('uses one visible-only fallback read rather than a timer per page', async () => {
    vi.useFakeTimers(); installMock(); const state = createDesktopState(); await state.initialize()
    expect(mockedInvoke.mock.calls.filter(([command]) => command === 'desktop_hook_health')).toHaveLength(1)
    mockedInvoke.mockClear(); await vi.advanceTimersByTimeAsync(10_000)
    expect(mockedInvoke.mock.calls.filter(([command]) => command === 'desktop_hook_health')).toHaveLength(1)
    state.dispose(); expect(vi.getTimerCount()).toBe(0); vi.useRealTimers()
  })
  it('prioritizes a repair requirement over historical verification', async () => {
    mockedInvoke.mockImplementation(async (command: string) => command === 'desktop_hook_health' ? health({ installation: 'needs_repair' }) as never : defaults(command) as never)
    const state = createDesktopState(); await state.initialize()
    expect(state.hookLabel.value).toBe('Hook 配置需要修复'); expect(state.hookTone.value).toBe('danger'); state.dispose()
  })

  it('does not apply a late Hook-health read after disposal', async () => {
    installMock()
    const state = createDesktopState()
    await state.initialize()
    let resolveHealth!: (value: HookHealth) => void
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_hook_health')
        return new Promise<HookHealth>((resolve) => {
          resolveHealth = resolve
        }) as never
      return defaults(command) as never
    })

    const refreshing = state.refreshHookStatus()
    state.dispose()
    resolveHealth(health({ installation: 'needs_repair', diagnosticCode: 'LATE_RECEIPT' }))
    await refreshing

    expect(state.hookHealth.value?.installation).toBe('current')
    expect(state.hookLabel.value).toBe('尚未验证实际触发')
  })
})

describe('launcher and scoped mutations', () => {
  it('preserves dirty launcher, policy, and Hook-home drafts across snapshots', async () => {
    installMock(); const state = createDesktopState(); await state.initialize()
    state.launcherDraft.value!.proxy.host = '127.0.0.9'; state.policyDraft.value!.completion_quiet_ms = 3100; state.home.value = 'C:\\draft'
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_status') return status({ policy: { ...policy(), completion_quiet_ms: 4200 }, policyRevision: 5, hookHome: 'C:\\saved' }) as never
      if (command === 'desktop_launcher_status') return launcher({ config: { proxy: { enabled: false, host: '127.0.0.2', port: 7890, noProxy: [] }, desktop: { selectedExecutable: null, refuseIfRunning: true } } }) as never
      if (command === 'desktop_hook_health') return health() as never
      if (command === 'desktop_relay_status') return relay() as never
      if (command === 'desktop_autostart_status') return false as never
      if (command === 'desktop_deliveries') return { items: [], nextCursor: null } as never
      return undefined as never
    })
    await state.refreshAll()
    expect(state.launcherDraft.value?.proxy.host).toBe('127.0.0.9'); expect(state.policyDraft.value?.completion_quiet_ms).toBe(3100); expect(state.home.value).toBe('C:\\draft'); expect(state.policyBaselineChanged.value).toBe(true); state.dispose()
  })
  it('saves a visible launcher draft before launching', async () => {
    installMock(); const state = createDesktopState(); await state.initialize(); state.launcherDraft.value!.desktop.selectedExecutable = 'C:\\ChatGPT.exe'; mockedInvoke.mockClear()
    mockedInvoke.mockImplementation(async (command: string, args?: unknown) => {
      if (command === 'desktop_launcher_save') return (args as { config: LauncherStatus['config'] }).config as never
      if (command === 'desktop_launcher_status') return launcher({ config: (args as never) }) as never
      if (command === 'desktop_launch') return null as never
      return defaults(command) as never
    })
    await state.launchDesktop()
    expect(mockedInvoke.mock.calls.map(([command]) => command)).toContain('desktop_launcher_save'); expect(mockedInvoke.mock.calls.map(([command]) => command)).toContain('desktop_launch'); state.dispose()
  })
  it('keeps a missing persisted executable repairable in settings', async () => {
    mockedInvoke.mockImplementation(async (command: string) => command === 'desktop_launcher_status' ? launcher({ config: { proxy: { enabled: false, host: '127.0.0.1', port: 7890, noProxy: [] }, desktop: { selectedExecutable: 'C:\\old\\ChatGPT.exe', refuseIfRunning: true } }, selectionIssue: { message: 'DESKTOP_EXECUTABLE_MISSING' }, candidates: [{ executable: 'C:\\new\\ChatGPT.exe', productLabel: 'ChatGPT Desktop', packageVersion: '1' }] }) as never : defaults(command) as never)
    const state = createDesktopState(); await state.initialize()
    expect(state.loadFailed.value).toBe(false); expect(state.launcher.value?.selectionIssue?.message).toBe('DESKTOP_EXECUTABLE_MISSING'); expect(state.launcherDraft.value?.desktop.selectedExecutable).toBe('C:\\old\\ChatGPT.exe'); state.dispose()
  })
  it('does not refresh Launcher after policy or Relay mutations', async () => {
    installMock(); const state = createDesktopState(); await state.initialize(); state.policyDraft.value!.notify_started = false; mockedInvoke.mockClear()
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_save_policy') return { status: 'saved', state: { policy: state.policyDraft.value, captureGeneration: 0, revision: 5 } } as never
      if (command === 'desktop_status') return status({ policy: state.policyDraft.value, policyRevision: 5 }) as never
      if (command === 'desktop_hook_health') return health() as never
      if (command === 'desktop_relay_configure') return null as never
      if (command === 'desktop_relay_status') return relay() as never
      if (command === 'desktop_deliveries') return { items: [], nextCursor: null } as never
      return false as never
    })
    await state.savePolicy(); expect(mockedInvoke).toHaveBeenCalledWith('desktop_save_policy', { policy: expect.objectContaining({ notify_started: false }), expectedRevision: 4 }); expect(mockedInvoke.mock.calls.some(([command]) => command === 'desktop_launcher_status')).toBe(false)
    mockedInvoke.mockClear(); state.relayUrl.value = 'https://relay.example.test'; state.deviceToken.value = 'token'; await state.configureRelay(); expect(mockedInvoke.mock.calls.some(([command]) => command === 'desktop_launcher_status')).toBe(false); state.dispose()
  })
})

describe('typed policy receipts', () => {
  it('rejects an unknown receipt without changing the saved baseline', async () => {
    installMock(); const state = createDesktopState(); await state.initialize(); state.policyDraft.value!.notify_started = false
    mockedInvoke.mockImplementation(async (command: string) => command === 'desktop_save_policy' ? null as never : defaults(command) as never)
    expect(await state.savePolicy()).toBe(false); expect(state.policyDirty.value).toBe(true); expect(state.policyBaseline.value?.notify_started).toBe(true); expect(state.message.value).toContain('回执无法确认'); state.dispose()
  })
  it('rejects a duplicate policy save while its typed receipt is pending', async () => {
    installMock(); const state = createDesktopState(); await state.initialize(); state.policyDraft.value!.notify_started = false; let resolve!: (value: unknown) => void
    mockedInvoke.mockImplementation(async (command: string) => command === 'desktop_save_policy' ? new Promise((done) => { resolve = done }) as never : defaults(command) as never)
    const first = state.savePolicy(); expect(await state.savePolicy()).toBe(false); expect(mockedInvoke.mock.calls.filter(([command]) => command === 'desktop_save_policy')).toHaveLength(1); resolve({ status: 'saved', state: { policy: state.policyDraft.value, captureGeneration: 0, revision: 5 } }); await first; state.dispose()
  })
  it('accepts saved_pending_apply and updates the authoritative baseline', async () => {
    installMock(); const state = createDesktopState(); await state.initialize(); state.policyDraft.value!.include_task_input = true
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_save_policy') return { status: 'saved_pending_apply', state: { policy: state.policyDraft.value, captureGeneration: 0, revision: 5 } } as never
      if (command === 'desktop_status') return status({ policy: state.policyDraft.value, policyRevision: 5, policyApplyStatus: 'pending' }) as never
      if (command === 'desktop_hook_health') return health() as never
      return defaults(command) as never
    })
    expect(await state.savePolicy()).toBe(true); expect(state.policyDirty.value).toBe(false); expect(state.status.value?.policyApplyStatus).toBe('pending'); expect(state.message.value).toContain('等待运行时应用'); state.dispose()
  })
  it('reports a completed policy mutation separately when its readback fails', async () => {
    installMock()
    const state = createDesktopState()
    await state.initialize()
    state.policyDraft.value!.notify_started = false
    const saved = { ...state.policyDraft.value! }
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_save_policy')
        return { status: 'saved', state: { policy: saved, captureGeneration: 0, revision: 5 } } as never
      if (command === 'desktop_status') throw new Error('readback unavailable')
      if (command === 'desktop_hook_health') return health() as never
      return defaults(command) as never
    })

    expect(await state.savePolicy()).toBe(true)
    expect(state.policyDirty.value).toBe(false)
    expect(state.policyBaseline.value?.notify_started).toBe(false)
    expect(state.message.value).toContain('通知设置已保存')
    expect(state.message.value).toContain('状态刷新失败')
    state.dispose()
  })
  it('keeps a changed draft on a conflict receipt', async () => {
    installMock(); const state = createDesktopState(); await state.initialize(); state.policyDraft.value!.notify_started = false
    mockedInvoke.mockImplementation(async (command: string) => command === 'desktop_save_policy' ? { status: 'conflict', state: { policy: policy(), captureGeneration: 0, revision: 5 }, errorCode: 'CONFIG_REVISION_CONFLICT' } as never : defaults(command) as never)
    expect(await state.savePolicy()).toBe(false); expect(state.policyDirty.value).toBe(true); expect(state.policyDraft.value?.notify_started).toBe(false); expect(state.policyBaseline.value?.notify_started).toBe(true); state.dispose()
  })
  it('uses the draft baseline revision after an external status refresh', async () => {
    installMock(); const state = createDesktopState(); await state.initialize(); state.policyDraft.value!.notify_started = false
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_status') return status({ policy: { ...policy(), notify_ended: false }, policyRevision: 5 }) as never
      if (command === 'desktop_hook_health') return health() as never
      if (command === 'desktop_launcher_status') return launcher() as never
      if (command === 'desktop_relay_status') return relay() as never
      if (command === 'desktop_autostart_status') return false as never
      if (command === 'desktop_deliveries') return { items: [], nextCursor: null } as never
      if (command === 'desktop_save_policy') return { status: 'conflict', state: { policy: { ...policy(), notify_ended: false }, captureGeneration: 0, revision: 5 }, errorCode: 'CONFIG_REVISION_CONFLICT' } as never
      return undefined as never
    })
    await state.refreshAll(); await state.savePolicy()
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_save_policy', { policy: expect.objectContaining({ notify_started: false }), expectedRevision: 4 })
    expect(state.policyDraft.value?.notify_started).toBe(false); expect(state.policyBaselineChanged.value).toBe(true); state.dispose()
  })
})

describe('delivery pagination', () => {
  it('keeps a late page after a head refresh and rejects an older row revision', async () => {
    let resolvePage!: (value: unknown) => void; let heads = 0
    mockedInvoke.mockImplementation(async (command: string, args?: unknown) => {
      if (command !== 'desktop_deliveries') return defaults(command) as never
      if ((args as { cursor?: string } | undefined)?.cursor === 'cursor-2') return new Promise((resolve) => { resolvePage = resolve }) as never
      heads += 1; return (heads === 1 ? { items: [delivery('d3', 1)], nextCursor: 'cursor-2' } : { items: [delivery('d3', 3, 'delivered')], nextCursor: 'cursor-2' }) as never
    })
    const state = createDesktopState(); await state.initialize(); const more = state.loadMoreDeliveries(); await state.refreshDeliveries(false); resolvePage({ items: [delivery('d3', 2), delivery('d2', 2)], nextCursor: null }); await more
    expect(state.deliveries.value.map((item) => item.id)).toEqual(['d3', 'd2']); expect(state.deliveries.value[0]?.status).toBe('delivered'); state.dispose()
  })
  it('caps metadata at 300 rows', async () => {
    const rows = Array.from({ length: 300 }, (_, index) => delivery(`d${300 - index}`, 300 - index)); installMock(); mockedInvoke.mockImplementation(async (command: string, args?: unknown) => {
      if (command === 'desktop_deliveries') return ((args as { cursor?: string } | undefined)?.cursor === 'older' ? { items: [delivery('d301', 301)], nextCursor: null } : { items: rows, nextCursor: 'older' }) as never
      return defaults(command) as never
    })
    const state = createDesktopState(); await state.initialize(); await state.loadMoreDeliveries(); expect(state.deliveries.value).toHaveLength(300); expect(state.deliveriesCapped.value).toBe(true); state.dispose()
  })
})

describe('subscriptions and Hook operations', () => {
  it('rejects old relay revisions and a late snapshot from an earlier epoch', async () => {
    const handlers = new Map<string, (event: { payload: RelayStatusCore }) => void>()
    mockedListen.mockImplementation(async (event, handler) => {
      handlers.set(event, handler as (event: { payload: RelayStatusCore }) => void)
      return () => undefined
    })
    let resolveSnapshot!: (value: RelayStatus) => void
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_relay_status')
        return new Promise<RelayStatus>((resolve) => {
          resolveSnapshot = resolve
        }) as never
      return defaults(command) as never
    })

    const state = createDesktopState()
    const initializing = state.initialize()
    await flushPromises()
    handlers.get('relay-status-changed')?.({
      payload: relay({ runtimeEpoch: 'epoch-b', revision: 4, state: 'unreachable' }),
    })
    handlers.get('relay-status-changed')?.({
      payload: relay({ runtimeEpoch: 'epoch-b', revision: 3, state: 'ready' }),
    })
    resolveSnapshot(relay({ runtimeEpoch: 'epoch-a', revision: 99, state: 'ready' }))
    await initializing

    expect(state.relay.value).toMatchObject({
      runtimeEpoch: 'epoch-b',
      revision: 4,
      state: 'unreachable',
    })
    state.dispose()
  })

  it('refreshes deliveries when its dirty event arrives', async () => {
    const handlers = new Map<string, () => void>()
    mockedListen.mockImplementation(async (event, handler) => {
      handlers.set(event, handler as () => void)
      return () => undefined
    })
    installMock()
    const state = createDesktopState()
    await state.initialize()
    mockedInvoke.mockClear()

    handlers.get('deliveries-changed')?.()
    await flushPromises()

    expect(mockedInvoke).toHaveBeenCalledWith('desktop_deliveries', { limit: 20, cursor: null })
    state.dispose()
  })

  it('uses visible delivery fallback after a subscription failure, while focus starts no network action', async () => {
    vi.useFakeTimers()
    mockedListen.mockImplementation(async (event) => {
      if (event === 'deliveries-changed') throw new Error('delivery listener unavailable')
      return () => undefined
    })
    installMock()
    const state = createDesktopState()
    await state.initialize()
    mockedInvoke.mockClear()

    await vi.advanceTimersByTimeAsync(30_000)
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_deliveries', { limit: 20, cursor: null })
    expect(mockedInvoke).not.toHaveBeenCalledWith('desktop_relay_probe')

    mockedInvoke.mockClear()
    window.dispatchEvent(new Event('focus'))
    await flushPromises()
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_deliveries', { limit: 20, cursor: null })
    expect(mockedInvoke).not.toHaveBeenCalledWith('desktop_relay_probe')
    expect(mockedInvoke).not.toHaveBeenCalledWith('desktop_relay_test')
    state.dispose()
    expect(vi.getTimerCount()).toBe(0)
    vi.useRealTimers()
  })

  it('registers listeners before taking the initial snapshots and disposes late listeners', async () => {
    let resolveListener!: (unlisten: () => void) => void; let released = false; let listenerCount = 0
    mockedListen.mockImplementation(() => {
      listenerCount += 1
      return listenerCount === 1 ? new Promise((resolve) => { resolveListener = resolve }) as never : Promise.resolve(() => undefined) as never
    }); installMock()
    const state = createDesktopState(); const initializing = state.initialize(); await Promise.resolve(); state.dispose(); resolveListener(() => { released = true }); await initializing
    expect(released).toBe(true)
  })
  it('plans and applies only a complete Hook plan', async () => {
    const plan = { registrationId: 'registration', operationId: 'operation', sourceFingerprint: null, expectedPolicy: policy(), changedEvents: [], definitionChanged: false, outcome: 'no_change', reviewRequired: false, externalReviewRequired: false }
    installMock(); const state = createDesktopState(); await state.initialize(); state.home.value = 'C:\\codex'
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_hook_plan' || command === 'desktop_install_hook') return plan as never
      if (command === 'desktop_status') return status({ hookHome: 'C:\\codex' }) as never
      if (command === 'desktop_hook_health') return health() as never
      return defaults(command) as never
    })
    expect(await state.planHook()).toBe(true); expect(await state.applyHookPlan()).toBe(true); expect(mockedInvoke).toHaveBeenCalledWith('desktop_install_hook', expect.objectContaining({ registrationId: 'registration' })); state.dispose()
  })
  it('rejects an incomplete Hook-install receipt and preserves the draft', async () => {
    const plan = { registrationId: 'registration', operationId: 'operation', sourceFingerprint: null, expectedPolicy: policy(), changedEvents: [], definitionChanged: false, outcome: 'no_change', reviewRequired: false, externalReviewRequired: false }
    installMock()
    const state = createDesktopState()
    await state.initialize()
    state.home.value = 'C:\\codex'
    mockedInvoke.mockImplementation(async (command: string) => {
      if (command === 'desktop_hook_plan') return plan as never
      if (command === 'desktop_install_hook') return { ...plan, expectedPolicy: undefined } as never
      return defaults(command) as never
    })

    expect(await state.planHook()).toBe(true)
    expect(await state.applyHookPlan()).toBe(false)
    expect(state.homeDirty.value).toBe(true)
    expect(state.message.value).toContain('回执无法确认')
    state.dispose()
  })
  it('keeps send and receipt permissions distinct', () => {
    const state = createDesktopState(); state.relay.value = { ...relay(), canSubmit: true, canReadOwn: false }; expect(state.relayLabel.value).toBe('可发送，回执读取权限不足'); state.relay.value = { ...relay(), state: 'permission_denied', canSubmit: false, canReadOwn: null }; expect(state.relayLabel.value).toBe('缺少通知发送权限'); state.dispose()
  })
})
