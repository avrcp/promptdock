import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createNotificationHoldController } from './notificationHold'
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }))
const mockedInvoke = vi.mocked(invoke)
const mockedListen = vi.mocked(listen)
const inactive = {
  revision: 3,
  startedAt: null,
  until: null,
  requestedDuration: null,
  state: 'inactive' as const,
}
beforeEach(() => {
  mockedInvoke.mockReset()
  mockedListen.mockResolvedValue(() => undefined)
  mockedInvoke.mockResolvedValue(inactive as never)
})
describe('notification hold controller', () => {
  it('disposes a late listener without starting a refresh timer', async () => {
    vi.useFakeTimers()
    let resolveListener!: (value: () => void) => void
    const unsubscribe = vi.fn()
    mockedListen.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveListener = resolve
        }),
    )
    const c = createNotificationHoldController()
    const start = c.start()
    c.dispose()
    resolveListener(unsubscribe)
    await start
    await vi.advanceTimersByTimeAsync(60_000)
    expect(unsubscribe).toHaveBeenCalledTimes(1)
    expect(mockedInvoke).not.toHaveBeenCalled()
    vi.useRealTimers()
  })
  it('does not let an older in-flight read replace a committed hold receipt', async () => {
    const c = createNotificationHoldController()
    await c.refresh()
    let resolveOld!: (value: unknown) => void
    mockedInvoke.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveOld = resolve
        }),
    )
    const oldRead = c.refresh()
    mockedInvoke.mockResolvedValueOnce({
      status: 'applied',
      state: {
        ...inactive,
        revision: 4,
        state: 'active',
        startedAt: 1,
        until: 900001,
        requestedDuration: 900000,
      },
    } as never)
    await c.hold(15)
    resolveOld(inactive)
    await oldRead
    expect(c.status.value?.revision).toBe(4)
    c.dispose()
  })
  it('uses a CAS revision and surfaces the actual state after conflict', async () => {
    const c = createNotificationHoldController()
    await c.refresh()
    mockedInvoke.mockResolvedValueOnce({
      status: 'conflict',
      state: {
        ...inactive,
        revision: 4,
        state: 'active',
        startedAt: 1,
        until: 2,
        requestedDuration: 900000,
      },
    } as never)
    await c.hold(15)
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_notification_hold_set', {
      minutes: 15,
      expectedRevision: 3,
    })
    expect(c.status.value?.revision).toBe(4)
    expect(c.error.value).toContain('其他位置')
    c.dispose()
  })
  it('re-reads after an unconfirmed timeout and never claims applied', async () => {
    const c = createNotificationHoldController()
    await c.refresh()
    mockedInvoke
      .mockRejectedValueOnce(new Error('timeout'))
      .mockResolvedValueOnce(inactive as never)
    await c.resume()
    expect(c.error.value).toContain('timeout')
    expect(
      mockedInvoke.mock.calls.filter(([command]) => command === 'desktop_notification_hold_status'),
    ).toHaveLength(2)
    c.dispose()
  })
})
